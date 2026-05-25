
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

logger = logging.getLogger(__name__)

# --- CONNECTION POOLING CONSTANTS ---
POOL_SIZE = 2
ROTATION_CHECK_INTERVAL = 5   # Seconds (Remote Check - Light Load)
HEARTBEAT_INTERVAL = 3        # Seconds (Local Check - Zero Load)
# After a known remote failure (probe round-trip or downstream tunnel using this
# host as a jump), treat the host as "not ready" for this many seconds so a
# silently-dead TCP can't be re-picked while the master is being rebuilt.
REMOTE_FAILURE_COOLDOWN = 20

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
        # Timestamp of the last KNOWN-bad remote round-trip (either our own
        # rotation probe or a downstream tunnel that failed using us as jump).
        # Drives a cooldown in is_master_ready so silently-dead TCPs aren't
        # picked again while the master is being torn down + rebuilt.
        self.last_remote_failure_ts = 0.0
        
        # Paths
        self.target_control_path = self.get_ssh_control_path(host)
        self.pool_control_paths = {
            i: f"{self.target_control_path}-{i}" for i in range(POOL_SIZE)
        }

    def is_master_ready(self) -> bool:
        """Read-only: True iff this host is enabled, its active master is Ready,
        AND we have no recent evidence the remote TCP is dead.

        The pool_status flag alone is unreliable for "really reachable" — the
        local ControlMaster process can stay alive (and `ssh -O check` keeps
        passing) for a long time after the underlying TCP is gone. We layer a
        short cooldown on top: any code path that observes a real remote
        failure stamps `last_remote_failure_ts`, which suppresses this host
        from being picked as a jump until the cooldown lapses or the master
        is rebuilt (`mark_remote_ok` clears the stamp)."""
        if not (self.active and self.pool_status.get(self.active_index) == "Ready"):
            return False
        if time.time() - self.last_remote_failure_ts < REMOTE_FAILURE_COOLDOWN:
            return False
        return True

    def mark_remote_failure(self):
        """Record that a real remote round-trip just failed against this host.
        Called by check_and_rotate and by TunnelManager when start() failed
        through this host as a jump."""
        self.last_remote_failure_ts = time.time()

    def mark_remote_ok(self):
        """Clear the failure cooldown — a real remote round-trip succeeded."""
        self.last_remote_failure_ts = 0.0

    def demote_master(self, index):
        """Forcibly tear down the master at `index` so the heartbeat loop
        rebuilds it. Used when we know the underlying TCP is gone but the
        local pexpect child is still alive (and would otherwise pass the
        local `ssh -O check` for a long time)."""
        child = self.pool.pop(index, None)
        if child is not None:
            try:
                if child.isalive():
                    child.close(force=True)
            except Exception:
                pass
        self.pool_status[index] = "Dead"
        try:
            cleanup_stale_connection(self.pool_control_paths[index],
                                     self.host, kill_zombies=False)
        except Exception:
            pass

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
        while self.running:
            if self.active:
                self.manage_pool_loop()
            else:
                self.status = "[dim]Stopped[/dim]"
                self.last_msg = "Inactive"
                self.cleanup_all()
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
        """Starts a specific master connection in the pool"""
        path = self.pool_control_paths[index]
        
        # Cleanup first (No zombie kill, only specific index)
        cleanup_stale_connection(path, self.host, kill_zombies=False)
        
        log_file = f"/tmp/auto2fa_ssh_master_{self.host}_{index}.log"
        ssh_options = (
            "-v "
            f"-E {log_file} "
            "-o StrictHostKeyChecking=no "
            "-o UserKnownHostsFile=/dev/null "
            "-o ServerAliveInterval=10 "
            "-o ServerAliveCountMax=2 "
            "-o ConnectTimeout=10 "
            "-o ControlMaster=auto "
            f"-o ControlPath={path} "  # Use specific pool path
            "-o ControlPersist=yes"
        )
        
        # Build argv as a list directly so paths containing spaces (e.g. a
        # home directory like "/Users/john doe/.ssh/...") don't get split.
        ssh_argv = [
            "-v",
            "-E", log_file,
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ServerAliveInterval=10",
            "-o", "ServerAliveCountMax=2",
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
                fresh_otp = generate_passcode_from_secret(self.otp_secret)
                child.sendline(fresh_otp)
                idx = child.expect([
                    r"\$", r"#",                  # 0, 1
                    r"Login incorrect", r"Permission denied", # 2, 3
                    r"[Pp]assword:",             # 4 (Loop back / Failure)
                    pexpect.TIMEOUT, pexpect.EOF  # 5, 6
                ], timeout=20)
            
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
                # A fresh master implies a fresh TCP — lift any stale cooldown
                # so tunnels can pick this host again right away.
                if index == self.active_index:
                    self.mark_remote_ok()
                return True
            else:
                # First-expect index layout: 0=Password, 1=Verification[c]ode,
                # 2=Token, 3=Verification code, 4=$, 5=#, 6=TIMEOUT, 7=EOF
                if not password_sent and not should_send_otp and idx == 7:
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Process exited immediately (EOF). Likely Can't Assign Address or Config Error.")
                elif not password_sent and not should_send_otp and idx == 6:
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Timed out waiting for any prompt (host unreachable?).")
                elif idx == 4:
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Server looped back to Password prompt (Wrong Creds/OTP?). FinalIdx={idx}")
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
                    
                    # Case 1: Missing from pool (Failed spawn or not started)
                    if i not in self.pool:
                        should_restart = True
                        
                    # Case 2: Dead process
                    elif not self.pool[i].isalive():
                        logger.warning(f"[{self.host}] Master #{i} died. Restarting...")
                        should_restart = True
                        
                    # Case 3: Process alive but Socket dead/hung (Heartbeat Check)
                    elif current_time - last_heartbeat > HEARTBEAT_INTERVAL:
                        path = self.pool_control_paths[i]
                        try:
                            # Quick check (1s timeout)
                            # We use 'ssh -O check' which verifies the master process is responding LOCALLY.
                            # This does NOT ping the server, so it is cheap and safe.
                            chk_cmd = ["ssh", "-O", "check", "-o", f"ControlPath={path}", self.host]
                            res = subprocess.run(chk_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
                            if res.returncode != 0:
                                logger.warning(f"[{self.host}] Master #{i} socket unresponsive. Restarting...")
                                should_restart = True
                        except Exception:
                            # Timeout or other error -> Unresponsive
                            logger.warning(f"[{self.host}] Master #{i} socket check timeout. Restarting...")
                            should_restart = True

                    if should_restart:
                        self.pool_status[i] = "Dead"
                        # Prevent tight loop on immediate failure (e.g. Systemic SSH failure)
                        time.sleep(2) 
                        # Restart synchronous to ensure capacity?
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
        self.start_master(index)

    def check_and_rotate(self):
        """Probe the active master with a real remote round-trip and react.

        CRITICAL: this master is shared with the user's own interactive
        sessions (that's the whole point of ControlMaster). Demoting it
        sends `ssh -O exit` which tears down EVERY multiplexed session,
        including the user's TUI / shell / tmux. So we only demote on
        UNAMBIGUOUS TCP-dead signals (Connection refused/reset/closed,
        Broken pipe, etc.) — in those cases the user's session is already
        broken at the TCP layer anyway. For ambiguous failures (timeout,
        MaxSessions exhaustion, generic non-zero exit) we just rotate the
        symlink to the spare slot AND stamp the cooldown so the tunnel
        layer won't pick us, but leave the master alive so the user's
        session can keep going."""
        active = self.active_index
        path = self.pool_control_paths[active]

        try:
            cmd = ["ssh", "-o", f"ControlPath={path}", self.host, "echo ok"]
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=3)
        except subprocess.TimeoutExpired:
            # Almost always a busy master (user has a TUI / tmux multiplexed
            # through this master and it's saturating I/O), not a dead TCP.
            # Stamp cooldown and rotate, but DO NOT kill — that would close
            # the user's session.
            logger.info(f"[{self.host}] Probe timed out — likely busy. Rotating only.")
            self.mark_remote_failure()
            self._rotate_to_spare(active, label="busy")
            return
        except Exception as e:
            logger.warning(f"[{self.host}] Probe errored ({e}) — cooldown only.")
            self.mark_remote_failure()
            return

        if res.returncode == 0:
            self.mark_remote_ok()
            return

        stderr = (res.stderr or "").lower()
        # Hard TCP-dead signals — the kernel told us the underlying TCP is
        # gone. Any user session on this master is already dead, so demoting
        # to rebuild does no additional damage.
        tcp_dead = any(s in stderr for s in (
            "broken pipe",
            "connection reset by peer",
            "connection closed by remote",
            "connection refused",
            "no route to host",
        ))

        if tcp_dead:
            logger.warning(f"[{self.host}] Master #{active} TCP dead "
                           f"({stderr.strip()[:80]}). Demoting + failover.")
            self.mark_remote_failure()
            self.demote_master(active)
            self._rotate_to_spare(active, label="failover")
            return

        # Anything else — MaxSessions full, transient server hiccup, etc.
        # Don't kill: rotate symlink + stamp short cooldown.
        logger.info(f"[{self.host}] Master #{active} probe non-zero "
                    f"({stderr.strip()[:80]}). Rotating only.")
        self.mark_remote_failure()
        self._rotate_to_spare(active, label="rotated")

    def _rotate_to_spare(self, active, label):
        """Switch the symlink to the spare pool slot if it's Ready. No-op if
        the spare is also down. Keeps the active master alive (so any
        in-flight user sessions on it can drain)."""
        other = (active + 1) % POOL_SIZE
        if self.pool_status.get(other) == "Ready":
            self.update_symlink(other)
            self.status = f"[green]Pool Active ({other})[/green]"
            self.last_msg = f"{label} {active}->{other}"

    def check_ssh_socket(self):
        return True # Handled by monitor loop
            
    def mount_host(self):
        """Attempts to mount the remote host using sshfs. Returns True iff mounted."""
        if not shutil.which("sshfs"):
            self.last_msg = "sshfs not installed"
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
