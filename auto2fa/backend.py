
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

def cleanup_stale_connection(control_path, host):
    """Aggressively cleans up any existing connection/socket for this host"""
    logger.info(f"[{host}] Cleaning up prior connections...")
    
    # 1. Try polite exit
    if os.path.exists(control_path):
        try:
            exit_cmd = ["ssh", "-o", f"ControlPath={control_path}", "-O", "exit", host]
            subprocess.run(exit_cmd, check=False, timeout=5, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception:
            pass

    # 2. Force remove socket
    if os.path.exists(control_path):
        try:
            os.remove(control_path)
        except OSError:
            pass

    # 3. Aggressive Kill
    try:
        pattern = f"ControlPath={control_path}"
        subprocess.run(["pkill", "-f", pattern], check=False)
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
        self.child = None
        self.control_path = self.get_ssh_control_path(host)

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
                self.run_connection_loop()
            else:
                self.status = "[dim]Stopped[/dim]"
                self.last_msg = "Inactive"
                time.sleep(0.5)

    def run_connection_loop(self):
        self.status = "[yellow]Starting...[/yellow]"
        cleanup_stale_connection(self.control_path, self.host)
        self.unmount_host() # Ensure clean slate
        
        # SSH options
        ssh_options = (
            "-o StrictHostKeyChecking=no "
            "-o UserKnownHostsFile=/dev/null "
            "-o ServerAliveInterval=10 "
            "-o ServerAliveCountMax=2 "
            "-o ConnectTimeout=10 "
            "-o ControlMaster=auto "
            f"-o ControlPath={self.control_path} "
            "-o ControlPersist=yes"
        )
        
        cmd = f"ssh {ssh_options} {self.host}"
        
        try:
            self.status = "[blue]Connecting...[/blue]"
            self.last_msg = "Spawning SSH..."
            
            try:
                rows, cols = os.popen('stty size', 'r').read().split()
                self.child = pexpect.spawn(cmd, encoding='utf-8', timeout=20, dimensions=(int(rows), int(cols)))
            except:
                self.child = pexpect.spawn(cmd, encoding='utf-8', timeout=20)

            # Login Logic
            index = self.child.expect([
                r"[Pp]assword:",
                r"[Vv]erification[Cc]ode:",
                r"[Tt]oken:",
                r"Verification code:",
                r"\$", r"#",
                pexpect.TIMEOUT, pexpect.EOF
            ], timeout=20)
            
            # Password
            if index == 0:
                self.last_msg = "Sending Password..."
                self.child.sendline(self.password)
                index = self.child.expect([
                    r"[Vv]erification[Cc]ode:",
                    r"[Tt]oken:",
                    r"Verification code:",
                    r"\$", r"#",
                    pexpect.TIMEOUT, pexpect.EOF
                ], timeout=20)

            # OTP
            if index in [0, 1, 2, 3]:
                self.last_msg = "Sending OTP..."
                fresh_otp = generate_passcode_from_secret(self.otp_secret)
                self.child.sendline(fresh_otp)
                index = self.child.expect([
                    r"\$", r"#",
                    r"Login incorrect", r"Permission denied",
                    pexpect.TIMEOUT, pexpect.EOF
                ], timeout=20)

            # Result
            if index in [0, 1, 4]: # Success
                self.status = "[green]Connected[/green]"
                self.last_msg = "Multiplex active"
                send_notification("Auto2FA", f"Connected to {self.host}")
                
                # --- AUTO MOUNT ---
                self.mount_host()
                # ------------------
                
                # Monitor Loop
                while self.active and self.running:
                    if not self.child.isalive():
                        self.last_msg = "Process died"
                        send_notification("Auto2FA", f"Connection lost (Process died): {self.host}")
                        break
                    
                    # Active Heartbeat Check
                    if not self.check_ssh_socket():
                         self.last_msg = "Socket dead"
                         send_notification("Auto2FA", f"Connection lost (Socket dead): {self.host}")
                         break

                    try:
                        # Check for EOF (Remote Close)
                        idx = self.child.expect([pexpect.TIMEOUT, pexpect.EOF], timeout=5) 
                        if idx == 1:
                            self.last_msg = "Remote closed"
                            send_notification("Auto2FA", f"Connection closed by remote: {self.host}")
                            break
                    except pexpect.TIMEOUT:
                        pass # All good
                        
            else:
                self.status = "[red]Failed[/red]"
                self.last_msg = "Auth fail or Timeout"
                send_notification("Auto2FA", f"Failed to connect to {self.host}")
                time.sleep(5) 

        except Exception as e:
            self.status = "[red]Error[/red]"
            self.last_msg = str(e)[:30]
            logger.error(f"{self.host} Error: {e}")
            send_notification("Auto2FA", f"Error connecting to {self.host}")
            time.sleep(5)
            
        finally:
            self.unmount_host() # Clean unmount
            if self.child and self.child.isalive():
                self.child.close()
            # Ensure socket is gone
            cleanup_stale_connection(self.control_path, self.host)

    def check_ssh_socket(self):
        """Returns True if the ControlMaster socket is active and responsive"""
        try:
            cmd = ["ssh", "-O", "check", "-o", f"ControlPath={self.control_path}", self.host]
            subprocess.run(cmd, check=True, timeout=5, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return True
        except Exception:
            return False
            
    def mount_host(self):
        """Attempts to mount the remote host using sshfs"""
        if not shutil.which("sshfs"):
            return 

        mount_point = os.path.expanduser(f"~/Mounts/{self.host}")
        os.makedirs(mount_point, exist_ok=True)
        
        # Check if already mounted
        if os.path.ismount(mount_point):
            return

        logger.info(f"Mounting {self.host} to {mount_point}")
        self.last_msg = "Mounting FS..."
        
        # sshfs command
        cmd = f"sshfs {self.host}:/ {mount_point} -o reconnect,ServerAliveInterval=15,volname={self.host},StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null"
        
        try:
            # We spawn independent pexpect process for mount
            child = pexpect.spawn(cmd, encoding='utf-8', timeout=15)
            index = child.expect([
                r"[Pp]assword:",
                r"[Vv]erification[Cc]ode:",
                pexpect.EOF,
                pexpect.TIMEOUT
            ])
            
            if index == 0: # Password
                child.sendline(self.password)
                child.expect(pexpect.EOF) 
            elif index == 1: # OTP
                otp = generate_passcode_from_secret(self.otp_secret)
                child.sendline(otp)
                child.expect(pexpect.EOF)
            
            if os.path.ismount(mount_point):
                 send_notification("Auto2FA", f"Mounted files for {self.host}")
                 self.last_msg = "Mounted & Active"
            
        except Exception as e:
            logger.error(f"Failed to mount {self.host}: {e}")
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
        config_path = os.path.expanduser("~/.ssh/passwords.json")
        with open(config_path, 'r') as f:
            data = json.load(f)
        return data
    except Exception as e:
        print(f"Failed to load config: {e}")
        return {}
