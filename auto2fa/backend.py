
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

def send_notification(title, message):
    """Sends a native macOS desktop notification"""
    try:
        # Escape quotes to prevent shell injection/errors
        safe_title = title.replace('"', '\\"')
        safe_message = message.replace('"', '\\"')
        cmd = f'osascript -e \'display notification "{safe_message}" with title "{safe_title}"\''
        subprocess.run(cmd, shell=True, check=False)
    except Exception as e:
        logger.error(f"Failed to send notification: {e}")

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
            # Try lsof first (Precise)
            # lsof -t returns just PID
            result = subprocess.run(["lsof", "-t", control_path], capture_output=True, text=True)
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
            # We use -f for full command line match
            logger.info(f"[{host}] Killing zombie SSH clients...")
            subprocess.run(["pkill", "-f", f"ssh .*{host}"], check=False)
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
        
        # Pooling State
        self.pool = {}        # {index: pexpect_child}
        self.pool_status = {} # {index: "Init/Ready/Dead"}
        self.active_index = 0
        
        # Paths
        self.target_control_path = self.get_ssh_control_path(host)
        self.pool_control_paths = {
            i: f"{self.target_control_path}-{i}" for i in range(POOL_SIZE)
        }

    def get_ssh_control_path(self, host):
        """Resolves the ControlPath that ssh expects to use for this host"""
        try:
            # query ssh for the configuration options
            cmd = ["ssh", "-G", host]
            result = subprocess.run(cmd, capture_output=True, text=True, check=True)
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
        
        cmd = f"ssh {ssh_options} {self.host}"
        
        try:
            self.last_msg = f"Init Spawn #{index}..."
            try:
                # Debugging spawn hang - forcing ignore of parent headers
                # using preexec_fn=os.setsid might help detach?
                cmd_parts = cmd.split()
                self.last_msg = f"Spawning #{index}..."
                child = pexpect.spawn(cmd_parts[0], cmd_parts[1:], encoding='utf-8', timeout=20)
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
                return True
            else:
                if not password_sent and not should_send_otp and idx == 6: # EOF shifted to 6
                    logger.warning(f"[{self.host}] Master #{index} Failed Login. Process exited immediately (EOF). Likely Can't Assign Address or Config Error.")
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
                    last_heartbeat = current_time
                
                # Rotation Check
                if time.time() - last_rotate_check > ROTATION_CHECK_INTERVAL:
                    self.check_and_rotate()
                    last_rotate_check = time.time()
                    
                time.sleep(1)
        except Exception as e:
            logger.error(f"[{self.host}] manage_pool_loop CRASHED: {e}")
            self.status = "[red]Pool Crashed[/red]"
            self.last_msg = str(e)[:30]

    def start_master_async(self, index):
        time.sleep(5) # Stagger start
        self.start_master(index)

    def check_and_rotate(self):
        """Checks if active master is full/unresponsive and rotates if needed"""
        active = self.active_index
        path = self.pool_control_paths[active]
        
        try:
            # Probe if it accepts new session
            cmd = ["ssh", "-o", f"ControlPath={path}", self.host, "echo ok"]
            # Timeout fast (3s). If MaxSessions full, it might hang or Refuse
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=3)
            
            # If return code is not 0, or if "Session open refused" in stderr
            if res.returncode != 0:
                logger.warning(f"[{self.host}] Active Master #{active} refused/failed. Rotating...")
                
                other = (active + 1) % POOL_SIZE
                
                # Check if other is ready?
                if self.pool_status.get(other) == "Ready":
                    self.update_symlink(other)
                    self.status = f"[green]Pool Active ({other})[/green]"
                    self.last_msg = f"Rotated {active}->{other}"
                    
                    # Note: We don't kill the full master. It might just be full.
                    # It will drain eventually.
        except Exception:
            # Timeout usually means full too?
            logger.warning(f"[{self.host}] Probe timed out. Rotating...")
            other = (active + 1) % POOL_SIZE
            if self.pool_status.get(other) == "Ready":
                self.update_symlink(other)
                self.status = f"[green]Pool Active ({other})[/green]"
                self.last_msg = f"Rotated {active}->{other}"

    def check_ssh_socket(self):
        return True # Handled by monitor loop
            
    def mount_host(self):
        """Attempts to mount the remote host using sshfs"""
        if not shutil.which("sshfs"):
            return 
            
        mount_point = os.path.expanduser(f"~/Mounts/{self.host}")
        os.makedirs(mount_point, exist_ok=True)
        if os.path.ismount(mount_point):
            return

        logger.info(f"Mounting {self.host} to {mount_point} via symlink")
        self.last_msg = "Mounting FS..."
        
        # Use target_control_path (Symlink)
        # sshfs should follow it if we point to it OR if config points to it.
        # User config points to target_control_path.
        
        cmd = f"sshfs {self.host}:/ {mount_point} -o reconnect,ServerAliveInterval=15,volname={self.host},StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null"
        
        try:
            # We assume passwordless because of control master
            subprocess.run(cmd, shell=True, timeout=10)
            
            if os.path.ismount(mount_point):
                 send_notification("Auto2FA", f"Mounted files for {self.host}")
                 self.last_msg = "Mounted"
        except Exception as e:
            logger.error(f"Failed to mount: {e}")
            self.last_msg = "Mount Failed"

    def unmount_host(self):
        """Unmounts the sshfs volume"""
        if not shutil.which("sshfs"):
            return

        mount_point = os.path.expanduser(f"~/Mounts/{self.host}")
        if not os.path.exists(mount_point):
            return
            
        try:
            cmd = ["umount", "-f", mount_point]
            subprocess.run(cmd, capture_output=True, timeout=5)
            
            if not os.path.ismount(mount_point):
                try:
                    os.rmdir(mount_point)
                except:
                    pass
        except Exception as e:
            logger.error(f"Failed to unmount {self.host}: {e}")

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
