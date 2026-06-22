
import json
import pyotp
import urllib.parse
import os
import sys
import pexpect
import time
import logging
import subprocess
import threading
import shutil
import hashlib

logger = logging.getLogger(__name__)

# --- CONNECTION POOLING CONSTANTS ---
POOL_SIZE = 2
ROTATION_CHECK_INTERVAL = 5   # Seconds (Remote Check - Light Load)
HEARTBEAT_INTERVAL = 3        # Seconds (Local Check - Zero Load)
HEARTBEAT_CHECK_TIMEOUT = 5   # Seconds. Timeout for the local `ssh -O check`
                              # probe. Generous on purpose: the probe is local
                              # and normally returns in ms, but a momentary
                              # local stall must NOT false-positive — a failed
                              # probe rebuilds the master, which SIGKILLs the
                              # socket holder, i.e. the user's live shell.

def send_notification(title, message):
    """Sends a native macOS desktop notification.

    Runs on a daemon thread with a timeout — osascript can hang for several
    seconds (Notification Center backlog, DND changes), and we must never
    block the host's manage_pool_loop on a cosmetic notification.
    """
    def _run():
        try:
            safe_title = title.replace('"', '\\"')
            safe_message = message.replace('"', '\\"')
            cmd = f'osascript -e \'display notification "{safe_message}" with title "{safe_title}"\''
            subprocess.run(cmd, shell=True, check=False, timeout=2,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception as e:
            logger.error(f"Failed to send notification: {e}")
    threading.Thread(target=_run, daemon=True).start()

def generate_passcode_from_secret(secret):
    try:
        otp = pyotp.TOTP(secret)
        return otp.now()
    except Exception as e:
        logger.error(f"Failed to generate OTP: {e}")
        raise


# --- Cross-host OTP replay guard --------------------------------------------
# Many users (e.g. all a large HPC center login nodes) configure every host with
# the same Duo TOTP secret. When the daemon brings several such hosts up in
# parallel, each spawns ssh, each derives the same 6-digit code, and the
# server consumes the first while rejecting the rest as replays — which we
# saw as "Server looped back to Password prompt" cascades across hosts within
# the same second.
#
# Guard plan:
#   1. Group hosts by hash(secret) so only those that share a secret block
#      each other. Hosts with distinct secrets run in parallel as before.
#   2. Serialize the *OTP submission* (not the whole login) per group with a
#      lock — the lock is held only across sendline + recording, not the
#      full multi-second expect.
#   3. After a submission, remember the code. The next caller regenerates;
#      if the regenerated code matches the just-used one, sleep until the
#      next 30-second TOTP window before submitting.
_OTP_REGISTRY_LOCK = threading.Lock()
_OTP_GROUP_LOCKS: dict = {}
_OTP_LAST_SUBMITTED: dict = {}   # group_key -> (code, ts)
_TOTP_WINDOW_SEC = 30


def _otp_group_key(secret: str) -> str:
    return hashlib.sha256(secret.encode()).hexdigest()[:16] if secret else ""


def _get_otp_group_lock(secret: str):
    key = _otp_group_key(secret)
    if not key:
        return None
    with _OTP_REGISTRY_LOCK:
        lock = _OTP_GROUP_LOCKS.get(key)
        if lock is None:
            lock = threading.Lock()
            _OTP_GROUP_LOCKS[key] = lock
    return lock


def _fresh_otp_or_wait(secret: str, host_label: str, lock=None) -> str:
    """Generate a TOTP code that has not just been submitted for this
    secret group. If it has been, wait for the next 30-second window.

    On return the caller-supplied `lock` (if any) is HELD, so the caller can
    sendline + _record_otp_submission atomically. While SLEEPING for the next
    window, however, we RELEASE the lock: a window-wait can be up to ~31s, and
    holding the shared cross-host OTP lock for that long serialized and stalled
    every other host sharing the secret. We re-acquire and re-check after the
    sleep, so the no-replay guarantee is preserved."""
    key = _otp_group_key(secret)
    while True:
        code = generate_passcode_from_secret(secret)
        last_code, last_ts = _OTP_LAST_SUBMITTED.get(key, (None, 0.0))
        # If the previous submission was for a different code OR the
        # window has clearly rolled over (>=35s ago), this code is safe.
        if code != last_code or (time.time() - last_ts) > (_TOTP_WINDOW_SEC + 5):
            return code
        # Same code — wait until the next TOTP window boundary, plus a
        # 1s buffer so the new window is fully established server-side.
        wait_for = _TOTP_WINDOW_SEC - (int(time.time()) % _TOTP_WINDOW_SEC) + 1
        logger.info(
            "[%s] OTP %s would replay last submission; waiting %ds for next TOTP window",
            host_label, code, wait_for,
        )
        if lock is not None:
            lock.release()
        try:
            time.sleep(wait_for)
        finally:
            if lock is not None:
                lock.acquire()


def _record_otp_submission(secret: str, code: str) -> None:
    key = _otp_group_key(secret)
    if key:
        _OTP_LAST_SUBMITTED[key] = (code, time.time())

def extract_secret_from_url(otpauth_url):
    try:
        decoded_url = urllib.parse.unquote(otpauth_url)
        if "secret=" not in decoded_url:
            raise ValueError("The OTP URL does not contain a 'secret' parameter.")
        secret = decoded_url.split("secret=")[1].split('&')[0]
        return secret
    except Exception as e:
        logger.error(f"Failed to extract secret: {e}")
        raise

def cleanup_stale_connection(control_path, host, kill_zombies=False):
    """Aggressively cleans up any existing connection/socket for this host"""
    logger.info(f"[{host}] Cleaning up prior connections (zombies={kill_zombies})...")
    
    # 1. Try polite exit
    if os.path.exists(control_path):
        try:
            # We add -v to see why it might fail, though we discard output here usually
            exit_cmd = ["ssh", "-o", f"ControlPath={control_path}", "-O", "exit", host]
            subprocess.run(exit_cmd, check=False, timeout=5, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception:
            pass

    # 2. Identify and kill process holding the socket (if socket still exists)
    if os.path.exists(control_path):
        try:
            # Try lsof first (Precise). Add a timeout — lsof can hang on
            # NFS or wedged sockets and would block the host thread otherwise.
            result = subprocess.run(["lsof", "-t", control_path],
                                    capture_output=True, text=True, timeout=5)
            pids = result.stdout.strip().split()
            if pids:
                logger.info(f"[{host}] Killing stale PIDs holding socket: {pids}")
                for pid in pids:
                    try:
                        os.kill(int(pid), 15) # SIGTERM
                        time.sleep(0.5)
                        # Force kill if still alive?
                        try:
                            os.kill(int(pid), 0)
                            os.kill(int(pid), 9) # SIGKILL
                        except OSError:
                            pass # Gone
                    except Exception:
                        pass
        except Exception as e:
            logger.warning(f"[{host}] lsof cleanup failed: {e}")

        # 3. Fallback: Force remove socket file
        try:
            if os.path.exists(control_path):
                os.remove(control_path)
                logger.info(f"[{host}] Removed stale socket file")
        except OSError as e:
            logger.error(f"[{host}] Failed to remove socket: {e}")

    # 4. Aggressive Zombie Kill
    if kill_zombies:
        try:
            # Pkill pattern: "ssh .* <host>"
            # We use -f for full command line match. Timeout because pkill -f
            # scans every process and can be slow under load.
            logger.info(f"[{host}] Killing zombie SSH clients...")
            subprocess.run(["pkill", "-f", f"ssh .*{host}"], check=False, timeout=5)
        except Exception:
            pass

class SSHHostManager(threading.Thread):
    def __init__(self, host, password, otp_secret):
        super().__init__()
        self.host = host
        self.password = password
        self.otp_secret = otp_secret
        self.active = False   # User desired state
        self.running = True   # Thread running state
        self.status = "Stopped"
        self.last_msg = "Ready"
        # Dedicated mount-state bit. Was previously inferred by string-sniffing
        # last_msg, which lost the indicator the moment manage_pool_loop wrote
        # a new last_msg like "Pool Active (0)".
        self.is_mounted = False

        # Pooling State
        self.pool = {}        # {index: pexpect_child}
        self.pool_status = {} # {index: "Init/Ready/Dead"}
        self.active_index = 0
        # Per-index lock — only one start_master can run for a given pool
        # slot at a time. Without this, start_master_async (the background
        # thread that warms up master 1 five seconds after startup) raced
        # the heartbeat loop (which also tries to bring up missing pool
        # slots): both spawned ssh, both sent the SAME OTP in the same 30s
        # window, the server consumed the first and rejected the second
        # as a duplicate → counter climbed → 5-minute cool-down.
        # This race was the dominant cause of the "always disconnecting"
        # symptom the user reported.
        self._start_locks = {i: threading.Lock() for i in range(POOL_SIZE)}

        # OTP rate-limit cool-down: when start_master sees the "server
        # looped back to Password prompt" pattern repeatedly, the server
        # is rate-limiting us (we hit this for real — 17 failed logins
        # in a row pushed the user's account into slow-response mode).
        # Hammering harder just makes it worse, so we sit out for 5 min.
        self.consecutive_login_failures = 0
        self.cooldown_until_ts = 0.0
        # Cool-down is a last-resort circuit breaker. With the per-index
        # start lock in place, repeated OTP failures should be very rare,
        # so the threshold is forgiving and the cool-down is short — long
        # enough to let the server's rate-limit window clear, short enough
        # that the user doesn't perceive the daemon as broken.
        self.OTP_FAILURE_THRESHOLD = 5
        self.OTP_COOLDOWN_SEC = 60

        # Pool rotation ping-pong detection. When BOTH pool slots are
        # equally broken (TCP dead, server unreachable), check_and_rotate
        # would otherwise flip the symlink every 5s forever — log spam +
        # wasted probe round-trips. We track the time of the last rotation;
        # if a probe fails right after a recent rotation, we know rotating
        # again is pointless — back off probing for a minute so heartbeat
        # has time to rebuild masters.
        self.last_rotate_ts = 0.0
        self.probe_backoff_until_ts = 0.0
        self.ROTATION_PING_PONG_WINDOW = 30
        self.PROBE_BACKOFF_SEC = 60

        # Paths
        self.target_control_path = self.get_ssh_control_path(host)
        self.pool_control_paths = {
            i: f"{self.target_control_path}-{i}" for i in range(POOL_SIZE)
        }

    def is_master_ready(self) -> bool:
        """Read-only: True iff this host is enabled AND its active master is Ready.
        Used by TunnelManager to pick a jump host."""
        return self.active and self.pool_status.get(self.active_index) == "Ready"

    def force_master_rebuild(self):
        """Tear down both pool entries so manage_pool_loop's heartbeat rebuilds
        them on the next iteration. Only called by explicit external triggers
        (e.g. Mac wake-from-sleep recovery) — NEVER from the heartbeat itself,
        because this sends `ssh -O exit` which kills any multiplexed user
        sessions. After wake the user's session TCP is already dead anyway, so
        we lose nothing by killing the master."""
        for i in range(POOL_SIZE):
            child = self.pool.pop(i, None)
            if child is not None:
                try:
                    if child.isalive():
                        child.close(force=True)
                except Exception:
                    pass
            self.pool_status[i] = "Dead"
            try:
                cleanup_stale_connection(self.pool_control_paths[i],
                                         self.host, kill_zombies=False)
            except Exception:
                pass
        self.last_msg = "Wake recovery — rebuilding masters"

    def get_ssh_control_path(self, host):
        """Resolves the ControlPath that ssh expects to use for this host"""
        try:
            # query ssh for the configuration options. timeout so we don't
            # hang the entire app startup on a wedged ssh / proxy config.
            cmd = ["ssh", "-G", host]
            result = subprocess.run(cmd, capture_output=True, text=True,
                                    check=True, timeout=5)
            for line in result.stdout.splitlines():
                if line.lower().startswith("controlpath "):
                    path = line.split(" ", 1)[1].strip()
                    if path.lower() == "none":
                        home = os.path.expanduser("~")
                        return os.path.join(home, ".ssh", f"cm-{host}")
                    return path
        except Exception as e:
            logger.error(f"Failed to resolve ControlPath: {e}")
        
        # Fallback
        home = os.path.expanduser("~")
        return os.path.join(home, ".ssh", f"cm-auto2fa-{host}")

    def run(self):
        was_active = False
        while self.running:
            if self.active:
                was_active = True
                self.manage_pool_loop()
            else:
                self.status = "[dim]Stopped[/dim]"
                self.last_msg = "Inactive"
                # Only run cleanup_all on the active → inactive EDGE.
                # Previously this fired every 0.5s for every disabled host,
                # spamming the log with "Cleaning up prior connections..."
                # (twice per iteration since POOL_SIZE=2) — 2 disabled
                # hosts produced ~8 log lines per second, ballooning the
                # daemon log to multi-MB within hours.
                if was_active:
                    self.cleanup_all()
                    was_active = False
                time.sleep(0.5)
        # Final cleanup on thread exit (e.g. app shutdown while host was active):
        # manage_pool_loop returns when running flips False, leaving the pool
        # alive. Reap it here so SSH ControlMaster processes don't outlive the app.
        try:
            self.cleanup_all()
        except Exception as e:
            logger.error(f"[{self.host}] final cleanup_all error: {e}")
        # Unmount any sshfs mount so a broken volume isn't left behind.
        try:
            self.unmount_host()
        except Exception as e:
            logger.error(f"[{self.host}] final unmount_host error: {e}")

    def cleanup_all(self):
        """Clean up everything"""
        # Remove symlink
        if os.path.islink(self.target_control_path):
            try:
                os.remove(self.target_control_path)
            except: pass
        elif os.path.exists(self.target_control_path):
            try:
                os.remove(self.target_control_path)
            except: pass
            
        # Kill children and sockets
        for i in range(POOL_SIZE):
            if i in self.pool and self.pool[i].isalive():
                try:
                    self.pool[i].close()
                except: pass
            cleanup_stale_connection(self.pool_control_paths[i], self.host, kill_zombies=False)
        self.pool = {}

    def update_symlink(self, index):
        """Points the target ControlPath symlink to the specified pool index"""
        target = self.target_control_path
        source = self.pool_control_paths[index]
        
        try:
            # Atomic replacement if possible, but os.symlink fails if exists
            tmp_link = f"{target}.tmp"
            if os.path.exists(tmp_link):
                os.remove(tmp_link)
            # Ensure absolute path for source
            abs_source = os.path.abspath(source)
            os.symlink(abs_source, tmp_link)
            os.replace(tmp_link, target)
            
            logger.info(f"[{self.host}] Rotated to Pool {index}")
            self.active_index = index
            return True
        except Exception as e:
            logger.error(f"[{self.host}] Failed to update symlink: {e}")
            return False

    def start_master(self, index):
        """Starts a specific master connection in the pool.

        Guarded by a per-index non-blocking lock so concurrent callers
        (e.g. start_master_async vs. the heartbeat noticing the slot is
        empty) cannot both spawn ssh and burn the same OTP within one
        30-second window. Whoever loses the race just returns False.
        """
        lock = self._start_locks.get(index)
        if lock is None or not lock.acquire(blocking=False):
            self.last_msg = f"Master #{index} start already in progress"
            logger.debug("[%s] Master #%d start skipped — already in progress", self.host, index)
            return False
        try:
            return self._start_master_impl(index)
        finally:
            lock.release()

    def _start_master_impl(self, index):
        path = self.pool_control_paths[index]

        # Cleanup first (No zombie kill, only specific index)
        cleanup_stale_connection(path, self.host, kill_zombies=False)
        
        log_file = f"/tmp/auto2fa_ssh_master_{self.host}_{index}.log"

        # Build argv as a list directly so paths containing spaces (e.g. a
        # home directory like "/Users/john doe/.ssh/...") don't get split.
        #
        # Keepalive tolerance (15s x 12 = 180s) is deliberately generous and
        # kept in sync with ssh_config_template. The master holds the single
        # shared TCP connection; every multiplexed channel — including the
        # user's interactive `ssh host` working shell — rides on it. If the
        # master self-terminates the instant the network blips, the user's
        # session dies with it and their work is lost. A short blip (wifi
        # handover, VPN renegotiation, laptop briefly asleep) must NOT tear
        # the master down. Genuinely-dead masters are still caught fast by the
        # local `ssh -O check` heartbeat + check_and_rotate, which rotate the
        # symlink to the spare so new logins stay instant.
        ssh_argv = [
            "-v",
            "-E", log_file,
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=12",
            "-o", "ConnectTimeout=10",
            "-o", "ControlMaster=auto",
            "-o", f"ControlPath={path}",
            "-o", "ControlPersist=yes",
            self.host,
        ]

        try:
            self.last_msg = f"Init Spawn #{index}..."
            try:
                self.last_msg = f"Spawning #{index}..."
                child = pexpect.spawn("ssh", ssh_argv, encoding='utf-8', timeout=20)
                self.last_msg = f"Spawned #{index}"
                
                # Debug Pexpect
                try:
                    f_debug = open(f"/tmp/pexpect_{self.host}_{index}.log", "w")
                    child.logfile = f_debug
                    self.last_msg = f"Log Open #{index}"
                except: pass
                
                self.pool[index] = child
            except Exception as e:
                self.last_msg = f"Spawn Fail: {str(e)[:10]}"
                raise e
            
            # Login Logic (Reused)
            idx = child.expect([
                r"[Pp]assword:",
                r"[Vv]erification[Cc]ode:",
                r"[Tt]oken:",
                r"Verification code:",
                r"\$", r"#",
                pexpect.TIMEOUT, pexpect.EOF
            ], timeout=20)
            
            password_sent = False
            if idx == 0: # Password
                child.sendline(self.password)
                password_sent = True
                idx = child.expect([
                    r"[Vv]erification[Cc]ode:", # 0
                    r"[Tt]oken:",               # 1
                    r"Verification code:",      # 2
                    r"\$", r"#",                # 3, 4
                    pexpect.TIMEOUT, pexpect.EOF # 5, 6
                ], timeout=20)
                
            # Determine if OTP is needed
            should_send_otp = False
            if password_sent:
                 # Indices from second expect: 0, 1, 2 are OTP
                 if idx in [0, 1, 2]:
                     should_send_otp = True
            else:
                 # Indices from first expect: 1, 2, 3 are OTP
                 if idx in [1, 2, 3]:
                     should_send_otp = True
                     
            if should_send_otp:
                # Serialize OTP submission across hosts that share this
                # TOTP secret. Without this, hosts brought up in parallel
                # send the same code within milliseconds and the server
                # consumes the first while rejecting the rest as replays.
                otp_lock = _get_otp_group_lock(self.otp_secret)
                if otp_lock is not None:
                    otp_lock.acquire()
                try:
                    # _fresh_otp_or_wait releases `otp_lock` while sleeping for
                    # the next TOTP window (so peers aren't stalled) and holds
                    # it again on return, so sendline + record stay atomic.
                    fresh_otp = _fresh_otp_or_wait(self.otp_secret, self.host, otp_lock)
                    child.sendline(fresh_otp)
                    _record_otp_submission(self.otp_secret, fresh_otp)
                finally:
                    if otp_lock is not None:
                        otp_lock.release()
                # Timeout 60s (was 20s): Cannon's MOTD is ~50 lines + slurm
                # stats table, and the server slows down for a few minutes
                # after a burst of failed logins. 20s timed out mid-banner
                # and we missed the trailing $ prompt, falsely reporting
                # "Master 0 Failed" even though ssh had actually logged in.
                idx = child.expect([
                    r"\$", r"#",                  # 0, 1
                    r"Login incorrect", r"Permission denied", # 2, 3
                    r"[Pp]assword:",             # 4 (Loop back / Failure)
                    pexpect.TIMEOUT, pexpect.EOF  # 5, 6
                ], timeout=60)
            
            # Check Success (Shell prompt)
            is_success = False
            if should_send_otp:
                if idx in [0, 1]: is_success = True
            else:
                if password_sent:
                     if idx in [3, 4]: is_success = True
                else:
                     if idx in [4, 5]: is_success = True
                         
            if is_success:
                self.pool_status[index] = "Ready"
                logger.info(f"[{self.host}] Master #{index} Ready")
                # Successful login resets the OTP rate-limit counter.
                self.consecutive_login_failures = 0
                self.cooldown_until_ts = 0.0
                return True
            else:
                # First-expect index layout: 0=Password, 1=Verification[c]ode,
                # 2=Token, 3=Verification code, 4=$, 5=#, 6=TIMEOUT, 7=EOF
                if not password_sent and not should_send_otp and idx == 7:
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Process exited immediately (EOF). Likely Can't Assign Address or Config Error.")
                elif not password_sent and not should_send_otp and idx == 6:
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Timed out waiting for any prompt (host unreachable?).")
                elif (should_send_otp and idx in (2, 3)) or idx == 4:
                    # Canonical server-side credential rejections in the OTP
                    # path: idx 2 = "Login incorrect", idx 3 = "Permission
                    # denied", idx 4 = looped back to the Password prompt.
                    # ALL THREE must count toward the rate-limit cool-down.
                    # Previously only idx==4 (loop-back) did, so a server that
                    # answers a bad password/OTP with "Login incorrect" or
                    # "Permission denied" (the common OpenSSH/PAM behavior)
                    # never tripped the 5-strike breaker — consecutive_login_
                    # failures stayed 0, the cool-down never armed, and the
                    # pool loop respawned ssh every ~2-3s forever with the same
                    # wrong creds, deepening the server-side rate limit.
                    _reason = {2: "Login incorrect", 3: "Permission denied"}.get(
                        idx, "looped back to Password prompt")
                    logger.warning(f"[{self.host}] Master #{index} Failed Login ({_reason}, Wrong Creds/OTP?). FinalIdx={idx}")
                    self.consecutive_login_failures += 1
                    if self.consecutive_login_failures >= self.OTP_FAILURE_THRESHOLD:
                        self.cooldown_until_ts = time.time() + self.OTP_COOLDOWN_SEC
                        mins = self.OTP_COOLDOWN_SEC // 60
                        self.last_msg = f"Cool-down {mins}m (server rate-limited)"
                        logger.warning(
                            f"[{self.host}] {self.consecutive_login_failures} consecutive "
                            f"login failures — entering {mins}-minute cool-down to avoid "
                            "deepening the server-side rate limit."
                        )
                else:
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Steps: Pwd={password_sent}, OTP={should_send_otp}, FinalIdx={idx}")

                self.pool_status[index] = "Failed"
                return False
                
        except Exception as e:
            logger.error(f"[{self.host}] Master #{index} Error: {e}")
            self.pool_status[index] = "Error"
            return False

    def cleanup_remote_server(self, index):
        """Kills any existing antigravity-server processes on the remote host"""
        path = self.pool_control_paths[index]
        try:
            logger.info(f"[{self.host}] Cleaning up remote antigravity-server...")
            # We use the established control master to run the cleanup
            cmd = ["ssh", "-o", f"ControlPath={path}", self.host, "pkill -f antigravity-server || true"]
            subprocess.run(cmd, capture_output=True, timeout=5)
        except Exception as e:
            logger.warning(f"[{self.host}] Remote cleanup failed (non-fatal): {e}")

    def manage_pool_loop(self):
        try:
            # OTP rate-limit cool-down: if recent logins triggered the
            # server's slow-response mode, sit out for OTP_COOLDOWN_SEC
            # before trying again. Retrying immediately would just deepen
            # the rate limit.
            if time.time() < self.cooldown_until_ts:
                remaining = int(self.cooldown_until_ts - time.time())
                self.status = f"[yellow]Cool-down {remaining}s[/yellow]"
                self.last_msg = f"Rate-limit cool-down ({remaining}s left)"
                time.sleep(min(5.0, max(0.5, remaining)))
                return

            self.status = "[yellow]Initializing Pool...[/yellow]"

            # 1. Initial Cleanup (Global) - Only here do we kill zombies
            # We dummy a control path since we are killing globally
            dummy_path = self.target_control_path
            cleanup_stale_connection(dummy_path, self.host, kill_zombies=True)

            # 2. Start Master 0
            if not self.start_master(0):
                self.status = "[red]Master 0 Failed[/red]"
                logger.error(f"[{self.host}] Master 0 Failed Start")
                # The loop will retry Master 0.
            else:
                # Master 0 started successfully.
                # Perfrom Remote Cleanup using this fresh connection
                self.cleanup_remote_server(0)
                
            # 3. Enable Service (Symlink -> 0 initially, loop will fix if dead)
            self.update_symlink(0)
            if self.pool_status.get(0) == "Ready":
                self.status = "[green]Pool Active (0)[/green]"
                send_notification("Auto2FA", f"Connected to {self.host}")
            
            # 4. Start Master 1 (Background)
            # We assume Master 0 blocked for ~10-20s, so spacing is natural.
            threading.Thread(target=self.start_master_async, args=(1,), daemon=True).start()
            
            # 5. Monitor Loop
            last_rotate_check = time.time()
            last_heartbeat = time.time()
            
            while self.active and self.running:
                current_time = time.time()
                
                # Check Health
                for i in range(POOL_SIZE):
                    should_restart = False

                    # Snapshot the pool entry once. Previously we did
                    # `if i not in self.pool` then `self.pool[i].isalive()`
                    # in separate statements — _wake_recover's
                    # force_master_rebuild on the asyncio thread can pop
                    # the entry between those two reads, raising KeyError
                    # and CRASHING the entire manage_pool_loop (host then
                    # stuck in "Pool Crashed" until manual toggle).
                    child = self.pool.get(i)

                    # Case 1: Missing from pool (Failed spawn / popped)
                    if child is None:
                        should_restart = True

                    # Case 2: Dead process
                    elif not child.isalive():
                        logger.warning(f"[{self.host}] Master #{i} died. Restarting...")
                        should_restart = True

                    # Case 3: Process alive but Socket dead/hung (Heartbeat Check)
                    elif current_time - last_heartbeat > HEARTBEAT_INTERVAL:
                        path = self.pool_control_paths[i]
                        try:
                            # We use 'ssh -O check' which verifies the master process is responding LOCALLY.
                            # This does NOT ping the server, so it is cheap and safe.
                            # Timeout is HEARTBEAT_CHECK_TIMEOUT (generous) so a
                            # momentary local stall doesn't false-positive and
                            # take down a healthy master + the user's shell.
                            chk_cmd = ["ssh", "-O", "check", "-o", f"ControlPath={path}", self.host]
                            res = subprocess.run(chk_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=HEARTBEAT_CHECK_TIMEOUT)
                            if res.returncode != 0:
                                logger.warning(f"[{self.host}] Master #{i} socket unresponsive. Restarting...")
                                should_restart = True
                        except Exception:
                            # Timeout or other error -> Unresponsive
                            logger.warning(f"[{self.host}] Master #{i} socket check timeout. Restarting...")
                            should_restart = True

                    if should_restart:
                        # In OTP cool-down: skip *this slot* but keep monitoring
                        # the other one. Don't trash pool_status (leave whatever
                        # the previous state was — a flapping "Dead" badge
                        # confuses the UI) and don't `return` out of the
                        # heartbeat (that used to take a healthy pool 0 offline
                        # just because pool 1 needed a restart).
                        if time.time() < self.cooldown_until_ts:
                            self.last_msg = (
                                f"Cool-down — {int(self.cooldown_until_ts - time.time())}s"
                            )
                            continue
                        self.pool_status[i] = "Dead"
                        # Prevent tight loop on immediate failure (e.g. Systemic SSH failure)
                        time.sleep(2)
                        self.start_master(i)

                        # Update symlink if we just revived the active index
                        if self.active_index == i:
                            other = (i + 1) % POOL_SIZE
                            if self.pool_status.get(other) == "Ready":
                                self.update_symlink(other)
                
                if current_time - last_heartbeat > HEARTBEAT_INTERVAL:
                    # Use time.time() not current_time — the loop body above
                    # may have done a 20s start_master, making current_time
                    # a stale "now" that would fire the heartbeat again
                    # immediately on the next iteration.
                    last_heartbeat = time.time()
                
                # Rotation Check
                if time.time() - last_rotate_check > ROTATION_CHECK_INTERVAL:
                    self.check_and_rotate()
                    last_rotate_check = time.time()
                    
                time.sleep(1)
        except Exception as e:
            logger.error(f"[{self.host}] manage_pool_loop CRASHED: {e}")
            self.status = "[red]Pool Crashed[/red]"
            # Back off before run() re-enters us, so a systemic failure
            # (wrong creds, network outage) doesn't hammer the server.
            time.sleep(5)
            self.last_msg = str(e)[:30]

    def start_master_async(self, index):
        time.sleep(5) # Stagger start
        # Re-check desired state AFTER the stagger sleep. The user can toggle
        # the host off (or the daemon can shut down) during these 5s; without
        # this guard we'd spawn an SSH ControlMaster for an inactive host —
        # leaking the master AND burning an OTP from the shared TOTP window.
        if not (self.active and self.running):
            logger.info(f"[{self.host}] start_master_async({index}) aborted — host no longer active")
            return
        self.start_master(index)

    def check_and_rotate(self):
        """Probe the active master with a real remote round-trip. If it
        fails, rotate to the spare. If a recent rotation was followed by
        ANOTHER failure (ping-pong), both slots are broken — back off
        probing for a minute to let the heartbeat rebuild fresh masters
        instead of flipping the symlink endlessly."""
        now = time.time()
        # Skip if we're in a back-off after detecting ping-pong.
        if now < self.probe_backoff_until_ts:
            return

        active = self.active_index
        path = self.pool_control_paths[active]

        def _do_rotate(reason: str):
            other = (active + 1) % POOL_SIZE
            # If we just rotated < ROTATION_PING_PONG_WINDOW ago and we're
            # rotating AGAIN, both slots are equally broken. Back off so
            # heartbeat can rebuild instead of pointlessly flipping.
            since_last = now - self.last_rotate_ts
            if since_last < self.ROTATION_PING_PONG_WINDOW:
                self.probe_backoff_until_ts = now + self.PROBE_BACKOFF_SEC
                self.last_msg = f"Both pools failing — backoff {self.PROBE_BACKOFF_SEC}s"
                logger.warning(
                    f"[{self.host}] rotation ping-pong detected "
                    f"({since_last:.1f}s since last rotate); backing off "
                    f"{self.PROBE_BACKOFF_SEC}s. heartbeat will rebuild."
                )
                return
            if self.pool_status.get(other) == "Ready":
                self.update_symlink(other)
                self.status = f"[green]Pool Active ({other})[/green]"
                self.last_msg = f"Rotated {active}->{other} ({reason})"
                self.last_rotate_ts = now

        try:
            # 10s (was 3s): 3s was way too aggressive for Cannon's loaded
            # login nodes — a slow round-trip ≠ dead master. We saw 257
            # probe timeouts in 2 hours doing zero useful work and
            # flapping the active-pool symlink endlessly.
            cmd = ["ssh", "-o", f"ControlPath={path}", self.host, "echo ok"]
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            if res.returncode != 0:
                # Explicit refusal (master genuinely broken, not just
                # slow) — rotate to the spare.
                logger.warning(f"[{self.host}] Active Master #{active} refused/failed. Rotating...")
                _do_rotate("refused")
        except Exception:
            # Timeout: server is slow, master is *probably* fine. Don't
            # rotate — the local `ssh -O check` in the heartbeat is the
            # canonical dead-master detector. Just record and move on.
            logger.info(f"[{self.host}] Probe slow (>10s round-trip); not rotating")
            self.last_msg = f"probe slow (>10s) — server load"

    def check_ssh_socket(self):
        return True # Handled by monitor loop
            
    def mount_host(self):
        """Attempts to mount the remote host using sshfs. Returns True iff mounted."""
        if not shutil.which("sshfs"):
            self.last_msg = "sshfs not installed"
            return False

        # Defensive: never let a hand-edited/legacy host name escape ~/Mounts
        # via '/' or '..' (HOST_ADD validates new names, but old passwords.json
        # entries predate that check).
        if "/" in self.host or ".." in self.host or self.host in (".", ""):
            self.last_msg = "invalid host name for mount"
            logger.error("refusing to mount unsafe host name %r", self.host)
            return False

        mount_point = os.path.expanduser(f"~/Mounts/{self.host}")
        os.makedirs(mount_point, exist_ok=True)
        if os.path.ismount(mount_point):
            self.is_mounted = True
            return True

        logger.info(f"Mounting {self.host} to {mount_point} via symlink")
        self.last_msg = "Mounting FS..."

        # Use argv list (no shell) — guards against host names containing
        # spaces, semicolons, or other shell metacharacters.
        argv = [
            "sshfs",
            f"{self.host}:/",
            mount_point,
            "-o",
            ",".join([
                "reconnect",
                "ServerAliveInterval=15",
                f"volname={self.host}",
                "StrictHostKeyChecking=no",
                "UserKnownHostsFile=/dev/null",
            ]),
        ]
        try:
            subprocess.run(argv, timeout=10,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            if os.path.ismount(mount_point):
                self.is_mounted = True
                send_notification("Auto2FA", f"Mounted files for {self.host}")
                self.last_msg = "Mounted"
                return True
            else:
                self.last_msg = "Mount Failed"
                return False
        except Exception as e:
            logger.error(f"Failed to mount: {e}")
            self.last_msg = "Mount Failed"
            return False

    def unmount_host(self):
        """Unmounts the sshfs volume. Returns True iff unmounted (or never mounted)."""
        if not shutil.which("sshfs"):
            return True

        mount_point = os.path.expanduser(f"~/Mounts/{self.host}")
        if not os.path.exists(mount_point) or not os.path.ismount(mount_point):
            self.is_mounted = False
            return True

        try:
            cmd = ["umount", "-f", mount_point]
            subprocess.run(cmd, capture_output=True, timeout=5)
            if not os.path.ismount(mount_point):
                self.is_mounted = False
                self.last_msg = "Unmounted"
                try:
                    os.rmdir(mount_point)
                except Exception:
                    pass
                return True
            self.last_msg = "Unmount Failed"
            return False
        except Exception as e:
            logger.error(f"Failed to unmount {self.host}: {e}")
            self.last_msg = "Unmount Failed"
            return False

    def toggle_mount(self):
        """User-facing mount toggle: mount if not mounted, else unmount.

        Debounced via self._mount_in_flight so rapid M presses don't fire
        simultaneous mount + unmount on the same host.
        """
        if not hasattr(self, "_mount_in_flight"):
            self._mount_in_flight = threading.Lock()
        if not self._mount_in_flight.acquire(blocking=False):
            return False  # another mount/unmount already running
        try:
            if self.is_mounted or os.path.ismount(
                os.path.expanduser(f"~/Mounts/{self.host}")
            ):
                return self.unmount_host()
            return self.mount_host()
        finally:
            self._mount_in_flight.release()

    def toggle(self):
        self.active = not self.active
        # Toggling clears any cool-downs — the user is explicitly retrying
        # so we shouldn't make them wait through a stale OTP cool-down or
        # a ping-pong back-off after they (presumably) fixed the underlying
        # issue (password updated, network restored, etc.).
        self.consecutive_login_failures = 0
        self.cooldown_until_ts = 0.0
        self.probe_backoff_until_ts = 0.0

def load_hosts():
    try:
        config_path = os.environ.get("SSH_CONFIG_PATH")
        # Ensure we look in the right place if env is missing?
        # Actually main.py loads hosts using its own load_hosts, this one might be unused.
        # But for completeness:
        if not config_path:
             config_path = os.path.expanduser("~/.ssh")
        
        with open(f"{config_path}/passwords.json", 'r') as f:
            data = json.load(f)
        return data
    except Exception as e:
        # print(f"Failed to load config: {e}")
        return {}
