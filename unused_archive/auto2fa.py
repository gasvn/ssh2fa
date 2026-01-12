import argparse
import json
import pyotp
import urllib.parse
from pathlib import Path
import os
import sys
import select
import pexpect
import time
import signal
import logging
import threading
import socket
import subprocess
import hashlib
from datetime import datetime, timedelta

# ANSI color codes for fancy output
class Colors:
    RESET = '\033[0m'
    PRIMARY = '\033[38;2;40;88;164m'
    ACCENT = '\033[38;2;28;130;180m'
    HIGHLIGHT = '\033[38;2;125;82;200m'
    SUCCESS = '\033[38;2;38;132;78m'
    WARNING = '\033[38;2;214;134;30m'
    DANGER = '\033[38;2;204;70;70m'
    INFO = '\033[38;2;86;96;120m'
    MUTED = '\033[38;2;120;126;140m'
    BOLD = '\033[1m'
    UNDERLINE = '\033[4m'


def style(text: str, *styles: str) -> str:
    """Return text wrapped in the given ANSI styles."""
    return f"{''.join(styles)}{text}{Colors.RESET}"


def print_banner():
    """Print the Auto2FA banner with a palette suitable for light terminals."""
    border = style("═" * 70, Colors.BOLD, Colors.PRIMARY)
    title = style("🔐  AUTO2FA", Colors.BOLD, Colors.HIGHLIGHT)
    subtitle = style("Smart SSH Login System", Colors.BOLD, Colors.ACCENT)
    features = style(
        "Auto-Reconnect · Network Switch · Sleep Recovery",
        Colors.BOLD,
        Colors.WARNING,
    )

    print()
    print(border)
    print(f"{title}  {subtitle}")
    print(border)
    print(features)
    print(border)
    print()

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s',
    handlers=[
        logging.FileHandler('/tmp/auto2fa.log'),
        logging.StreamHandler()
    ]
)
logger = logging.getLogger(__name__)

# Global variables for connection management
current_connection = None
reconnection_thread = None
connection_lock = threading.Lock()
should_reconnect = True
control_socket_path = None
CONTROL_PERSIST_SECONDS = 6 * 60 * 60  # 6 hours persistent control master
control_master_announced = False
last_totp_code = None
DEFAULT_MONITOR_REFRESH = 2.0
DEFAULT_MONITOR_LOG_LINES = 12


def ensure_control_socket_dir():
    """Ensure control socket directory exists and return its path."""
    control_dir = Path.home() / ".ssh" / "auto2fa_control"
    control_dir.mkdir(parents=True, exist_ok=True)
    return control_dir


def build_control_socket_path(host):
    """Build a deterministic, short control socket path for the host."""
    safe_host = ''.join(c if c.isalnum() or c in ('-', '_', '.') else '_' for c in host)
    host_hash = hashlib.sha1(host.encode('utf-8')).hexdigest()[:10]
    control_dir = ensure_control_socket_dir()
    socket_name = f"{safe_host}_{host_hash}.sock"
    return str(control_dir / socket_name)


def check_control_master(host, socket_path):
    """Check if an SSH control master is running for the given socket."""
    if not socket_path or not Path(socket_path).exists():
        return False

    try:
        result = subprocess.run(
            [
                'ssh',
                '-S', socket_path,
                '-O', 'check',
                host
            ],
            capture_output=True,
            text=True,
            timeout=10
        )
        if result.returncode == 0:
            logger.debug(f"Control master active for {host}: {result.stdout.strip()}")
            return True
        else:
            logger.warning(
                f"Control master check failed for {socket_path}: {result.stderr.strip()}"
            )
            return False
    except Exception as exc:
        logger.warning(f"Control master check error for {host}: {exc}")
        return False


def stop_control_master(host, socket_path):
    """Gracefully stop the SSH control master if it exists."""
    if not socket_path:
        return

    if Path(socket_path).exists():
        try:
            subprocess.run(
                ['ssh', '-S', socket_path, '-O', 'exit', host],
                capture_output=True,
                text=True,
                timeout=10
            )
        except Exception as exc:
            logger.warning(f"Failed to signal control master exit: {exc}")

    try:
        Path(socket_path).unlink(missing_ok=True)
    except Exception as exc:
        logger.debug(f"Unable to remove control socket {socket_path}: {exc}")


def start_control_master(host, password, otp_secret, retries=3, last_code=None):
    """Establish a persistent SSH control master for the host."""
    global control_socket_path, last_totp_code

    control_socket_path = build_control_socket_path(host)

    if check_control_master(host, control_socket_path):
        logger.info(f"Control master already active for {host} at {control_socket_path}")
        return control_socket_path

    # Remove any stale socket file
    try:
        Path(control_socket_path).unlink(missing_ok=True)
    except Exception as exc:
        logger.debug(f"Could not remove stale control socket {control_socket_path}: {exc}")

    password_patterns = [
        r"Password:",
        r"password:",
        r".*password.*:",
        r"Enter password for .*:"
    ]

    otp_patterns = [
        r"VerificationCode:",
        r"Verification code:",
        r".*verification.*code.*:",
        r"Enter.*code.*:",
        r"Token:",
        r".*token.*:"
    ]

    failure_patterns = [
        r"Permission denied",
        r"Too many authentication failures",
        r"Authentication failed",
        r"Connection closed by",
        r"Connection reset",
    ]

    all_patterns = password_patterns + otp_patterns + failure_patterns + [pexpect.TIMEOUT, pexpect.EOF]

    attempt = 0
    backoff_delay = 1

    while attempt < retries:
        try:
            fresh_otp = generate_passcode_from_secret(otp_secret, avoid_code=last_code)
            last_code = fresh_otp

            cmd = (
                f"ssh -f -N "
                f"-o BatchMode=no "
                f"-o StrictHostKeyChecking=no "
                f"-o UserKnownHostsFile=/dev/null "
                f"-o ControlMaster=yes "
                f"-o ControlPersist={CONTROL_PERSIST_SECONDS} "
                f"-o ControlPath={control_socket_path} "
                f"-o ServerAliveInterval=30 "
                f"-o ServerAliveCountMax=3 "
                f"{host}"
            )

            logger.info(f"Establishing control master for {host} (attempt {attempt + 1}/{retries})")
            child = pexpect.spawn(cmd, encoding='utf-8', timeout=45)

            while True:
                index = child.expect(all_patterns, timeout=45)

                if index < len(password_patterns):
                    logger.debug("Control master password prompt detected")
                    child.sendline(password)
                    continue

                if index < len(password_patterns) + len(otp_patterns):
                    logger.debug("Control master OTP prompt detected")
                    child.sendline(fresh_otp)
                    continue

                if index < len(password_patterns) + len(otp_patterns) + len(failure_patterns):
                    failure_msg = failure_patterns[index - len(password_patterns) - len(otp_patterns)]
                    logger.error(f"Control master error: {failure_msg}")
                    child.close()
                    break

                if index == len(all_patterns) - 2:  # TIMEOUT
                    logger.warning("Control master setup timeout, retrying...")
                    child.close()
                    break

                if index == len(all_patterns) - 1:  # EOF
                    child.close()
                    if check_control_master(host, control_socket_path):
                        logger.info(f"Control master ready at {control_socket_path}")
                        last_totp_code = fresh_otp
                        return control_socket_path
                    else:
                        logger.warning("Control master EOF but check failed, retrying...")
                        break

        except Exception as exc:
            logger.error(f"Control master setup exception: {exc}")

        attempt += 1
        time.sleep(backoff_delay)
        backoff_delay = min(backoff_delay * 2, 30)

    raise RuntimeError(f"Failed to establish control master for {host} after {retries} attempts")

# Function to check network connectivity (simplified, non-blocking)
def check_network_connectivity():
    """Check if basic network is available - lightweight check only"""
    try:
        # Quick check for internet connectivity
        socket.create_connection(("8.8.8.8", 53), timeout=2)
        logger.debug("Network connectivity confirmed")
        return True
    except:
        logger.debug("Network connectivity check failed")
        return False

# Function to detect system wake from sleep
def detect_system_wake():
    """Detect if system just woke from sleep by checking system uptime"""
    try:
        # Get system boot time
        result = subprocess.run(['sysctl', '-n', 'kern.boottime'], 
                              capture_output=True, text=True)
        if result.returncode == 0:
            # This is a simplified approach - in practice you might want to 
            # track the last known time and compare
            return True
    except:
        pass
    return False

# Function to extract password and OTP URL from the JSON password file
def get_password_config_for_host(host, password_file_path):
    try:
        with open(password_file_path, 'r') as f:
            passwords = json.load(f)
            return passwords.get(host)  # Returns { password, otpauthUrl }
    except Exception as e:
        print(f"Error reading password config file: {e}")
        sys.exit(1)

# Function to generate OTP based on OTP URL
def generate_passcode(otpauth_url):
    try:
        # Decode the URL if necessary
        decoded_url = urllib.parse.unquote(otpauth_url)  # URL decode to handle any % encoded characters
        logger.info(f"Decoded OTP URL: {decoded_url}")
        
        # Extract the secret from the URL (after ?secret=)
        if "secret=" not in decoded_url:
            raise ValueError("The OTP URL does not contain a 'secret' parameter.")
        
        secret = decoded_url.split("secret=")[1].split('&')[0]  # Handle additional parameters
        logger.info(f"Extracted Secret: {secret}")
        
        # Generate the OTP
        otp = pyotp.TOTP(secret)
        return otp.now()
    except Exception as e:
        logger.error(f"Failed to generate OTP: {e}")
        raise

# Function to generate OTP from secret directly
def generate_passcode_from_secret(secret, avoid_code=None, wait_timeout=45):
    """Generate OTP from secret string, optionally waiting for a new code."""
    try:
        otp = pyotp.TOTP(secret)
        code = otp.now()

        if avoid_code is None or code != avoid_code:
            return code

        # Wait for the next TOTP window if we need a fresh code
        logger.info("Current OTP matches the last code, waiting for rollover...")
        deadline = time.time() + wait_timeout
        while time.time() < deadline:
            time.sleep(1)
            code = otp.now()
            if code != avoid_code:
                logger.info("New OTP code generated after waiting.")
                return code

        logger.warning("Timed out waiting for a new OTP code; reusing current code.")
        return code
    except Exception as e:
        logger.error(f"Failed to generate OTP from secret: {e}")
        raise

# Function to extract secret from OTP URL
def extract_secret_from_url(otpauth_url):
    """Extract the secret from OTP URL for reuse"""
    try:
        decoded_url = urllib.parse.unquote(otpauth_url)
        if "secret=" not in decoded_url:
            raise ValueError("The OTP URL does not contain a 'secret' parameter.")
        secret = decoded_url.split("secret=")[1].split('&')[0]
        return secret
    except Exception as e:
        logger.error(f"Failed to extract secret from URL: {e}")
        raise

# Function to monitor system for wake events (macOS specific)
def setup_system_wake_monitoring():
    """Set up monitoring for system wake events on macOS"""
    try:
        # This is a simplified approach - you could enhance this with 
        # proper macOS power management notifications
        logger.info("System wake monitoring initialized")
        return True
    except Exception as e:
        logger.warning(f"Could not setup wake monitoring: {e}")
        return False

# Function to handle network interface changes
def monitor_network_changes():
    """Monitor for network interface changes that might affect SSH connections"""
    try:
        # Get current network interfaces
        result = subprocess.run(['ifconfig'], capture_output=True, text=True, timeout=5)
        if result.returncode == 0:
            return result.stdout
    except Exception as e:
        logger.warning(f"Could not monitor network changes: {e}")
    return None

# Function to create connection recovery strategy
def create_recovery_strategy(host, error_type):
    """Create appropriate recovery strategy based on error type"""
    strategies = {
        'network': {'delay': 10, 'retries': 5, 'backoff': 2.0},
        'auth': {'delay': 5, 'retries': 3, 'backoff': 1.5},
        'timeout': {'delay': 5, 'retries': 4, 'backoff': 1.2},
        'general': {'delay': 3, 'retries': 3, 'backoff': 1.5}
    }
    
    return strategies.get(error_type, strategies['general'])

# Function to read SSH config file and extract hostnames
def read_ssh_config(ssh_config_path):
    try:
        hosts = []
        with open(ssh_config_path, 'r') as f:
            lines = f.readlines()
            current_host = None
            for line in lines:
                line = line.strip()
                if line.startswith("Host "):
                    current_host = line.split()[1]
                    hosts.append(current_host)
        return hosts
    except Exception as e:
        print(f"Error reading SSH config: {e}")
        sys.exit(1)

# Enhanced SSH login function with robust error handling
def ssh_login_with_interactive_prompt(host, password, otp_secret, retries=5, keep_alive=True):
    """
    Enhanced SSH login with robust error handling for network issues, 
    sleep/wake cycles, and connection failures
    """
    global current_connection, control_master_announced, last_totp_code
    attempt = 0
    backoff_delay = 1
    
    while attempt < retries:
        try:
            # Only check basic network if previous attempts failed
            if attempt > 0 and not check_network_connectivity():
                logger.warning(f"Network issue detected. Waiting {backoff_delay} seconds...")
                time.sleep(backoff_delay)
                backoff_delay = min(backoff_delay * 2, 30)
                attempt += 1
                continue
            
            # Generate fresh OTP for each attempt
            fresh_otp = generate_passcode_from_secret(otp_secret, avoid_code=last_totp_code)
            
            # Enhanced SSH command with better connection options
            cmd = (f"ssh -t -o StrictHostKeyChecking=no "
                  f"-o UserKnownHostsFile=/dev/null "
                  f"-o ServerAliveInterval=30 "
                  f"-o ServerAliveCountMax=3 "
                  f"-o ConnectTimeout=10 "
                  f"-o TCPKeepAlive=yes {host}")
            
            logger.info(f"Attempting to login to {host} (attempt {attempt + 1}/{retries})...")
            print(f"  \033[1;36m⏳ Attempt {attempt + 1}/{retries}...\033[0m", end=" ", flush=True)

            # Start the SSH session with pexpect
            child = pexpect.spawn(cmd, encoding='utf-8')
            child.timeout = 45  # Longer timeout for initial connection

            # Enhanced prompt matching with more patterns
            password_patterns = [
                r"Password:",
                r"password:",
                r".*password.*:",
                r"Enter password for .*:"
            ]
            
            otp_patterns = [
                r"VerificationCode:",
                r"Verification code:",
                r".*verification.*code.*:",
                r"Enter.*code.*:",
                r"Token:",
                r".*token.*:"
            ]
            
            success_patterns = [
                r"\$",
                r"#",
                r"~.*\$",
                r".*@.*:.*\$",
                r".*@.*:.*#"
            ]
            
            all_patterns = password_patterns + otp_patterns + success_patterns + [pexpect.TIMEOUT, pexpect.EOF]

            # First expect - could be password, OTP, or direct success
            index = child.expect(all_patterns, timeout=45)

            # Handle password prompt
            if index < len(password_patterns):
                logger.info("Password prompt detected.")
                print("\033[1;32m🔑 Password\033[0m", flush=True)
                child.sendline(password)
                # Wait for next prompt
                index = child.expect(all_patterns, timeout=30)

            # Handle OTP prompt
            if index >= len(password_patterns) and index < len(password_patterns) + len(otp_patterns):
                logger.info("OTP prompt detected.")
                print(f"\033[1;35m🔐 2FA Code\033[0m", flush=True)
                child.sendline(fresh_otp)
                # Wait for login success
                index = child.expect(all_patterns, timeout=30)

            # Check for successful login
            if index >= len(password_patterns) + len(otp_patterns) and index < len(all_patterns) - 2:
                logger.info(f"Login successful for {host}.")
                print("\033[1;32m✅ Connected!\033[0m")
                current_connection = child
                last_totp_code = fresh_otp
                try:
                    socket_path = start_control_master(host, password, otp_secret, last_code=last_totp_code)
                    if not control_master_announced and socket_path:
                        print(
                            f"\033[1;34m🔁 SSH control master ready:\033[0m {socket_path}\n"
                            "   Configure VSCode/SSH to reuse this control socket for password-less reuse."
                        )
                        control_master_announced = True
                except Exception as exc:
                    logger.error(f"Failed to establish control master: {exc}")
                if keep_alive:
                    maintain_ssh_session_robust(child, host, password, otp_secret)
                return child
            
            # Handle timeout
            elif index == len(all_patterns) - 2:  # TIMEOUT
                logger.warning(f"SSH connection timeout for {host}. Retrying...")
                print("\033[1;33m⏰ Timeout\033[0m")
                child.close()
                attempt += 1
                time.sleep(backoff_delay)
                backoff_delay = min(backoff_delay * 1.5, 30)
                
            # Handle EOF
            elif index == len(all_patterns) - 1:  # EOF
                logger.warning(f"Connection closed unexpectedly for {host}. Retrying...")
                print("\033[1;31m❌ Closed\033[0m")
                child.close()
                attempt += 1
                time.sleep(backoff_delay)
                backoff_delay = min(backoff_delay * 1.5, 30)

        except pexpect.exceptions.ExceptionPexpect as e:
            logger.error(f"Pexpect error: {e}")
            attempt += 1
            time.sleep(backoff_delay)
            backoff_delay = min(backoff_delay * 2, 30)
            
        except Exception as e:
            logger.error(f"SSH login error: {e}")
            attempt += 1
            time.sleep(backoff_delay)
            backoff_delay = min(backoff_delay * 2, 30)

    logger.error(f"Failed to login after {retries} attempts.")
    return None

def maintain_ssh_session_robust(child, host, password, otp_secret, *, set_signal_handlers=True):
    """
    Maintain SSH session with robust error handling and automatic reconnection
    Handles network changes, sleep/wake cycles, and long periods of inactivity
    """
    global current_connection, should_reconnect, control_socket_path
    
    print(f"\n\033[1;36m🔄 Starting session maintenance for {host}...\033[0m")
    logger.info("Starting robust SSH session maintenance...")
    last_activity = datetime.now()
    keepalive_interval = 30  # seconds
    max_idle_time = 300  # 5 minutes before forced reconnect
    consecutive_failures = 0
    max_consecutive_failures = 3
    
    def signal_handler(signum, frame):
        global should_reconnect
        logger.info("Received termination signal, cleaning up...")
        should_reconnect = False
        if current_connection:
            current_connection.close()
        sys.exit(0)

    # Set up signal handlers for graceful shutdown when allowed
    if set_signal_handlers:
        signal.signal(signal.SIGINT, signal_handler)
        signal.signal(signal.SIGTERM, signal_handler)
    
    try:
        while should_reconnect:
            with connection_lock:
                if not current_connection or not current_connection.isalive():
                    logger.warning("Connection lost, attempting to reconnect...")
                    current_connection = ssh_login_with_interactive_prompt(
                        host, password, otp_secret, retries=3, keep_alive=False
                    )
                    if not current_connection:
                        logger.error("Failed to reconnect, retrying in 60 seconds...")
                        time.sleep(60)
                        continue
                    consecutive_failures = 0
                    last_activity = datetime.now()

                # Ensure control master persists for VSCode reuse
                if control_socket_path and not check_control_master(host, control_socket_path):
                    logger.warning("Control master missing, re-establishing...")
                    try:
                        start_control_master(host, password, otp_secret, last_code=last_totp_code)
                    except Exception as exc:
                        logger.error(f"Failed to restart control master: {exc}")
            
            try:
                # Check if too much time has passed since last activity
                if datetime.now() - last_activity > timedelta(seconds=max_idle_time):
                    logger.info("Maximum idle time reached, refreshing connection...")
                    with connection_lock:
                        if current_connection:
                            current_connection.close()
                        current_connection = ssh_login_with_interactive_prompt(
                            host, password, otp_secret, retries=3, keep_alive=False
                        )
                        if current_connection:
                            last_activity = datetime.now()
                            consecutive_failures = 0
                        else:
                            consecutive_failures += 1
                
                # Send keepalive command
                if current_connection and current_connection.isalive():
                    current_connection.sendline("echo 'keepalive'")
                    
                    # Set up patterns for response
                    patterns = [
                        r"keepalive",
                        r"\$",
                        r"#",
                        pexpect.TIMEOUT,
                        pexpect.EOF
                    ]
                    
                    index = current_connection.expect(patterns, timeout=10)
                    
                    if index < 3:  # Got expected response
                        logger.debug("Keepalive successful")
                        last_activity = datetime.now()
                        consecutive_failures = 0
                    elif index == 3:  # TIMEOUT
                        logger.warning("Keepalive timeout")
                        consecutive_failures += 1
                    else:  # EOF
                        logger.warning("Connection closed during keepalive")
                        with connection_lock:
                            current_connection = None
                        consecutive_failures += 1
                
                # Check if we've had too many consecutive failures
                if consecutive_failures >= max_consecutive_failures:
                    logger.error("Too many consecutive failures, forcing reconnection...")
                    with connection_lock:
                        if current_connection:
                            current_connection.close()
                        current_connection = None
                    consecutive_failures = 0
                    continue
                
                # Wait before next keepalive
                time.sleep(keepalive_interval)
                
            except pexpect.exceptions.EOF:
                logger.warning("SSH session terminated unexpectedly")
                with connection_lock:
                    current_connection = None
                consecutive_failures += 1
                
            except pexpect.exceptions.TIMEOUT:
                logger.warning("SSH session timeout")
                consecutive_failures += 1
                
            except Exception as e:
                logger.error(f"Error in session maintenance: {e}")
                consecutive_failures += 1
                time.sleep(5)
    
    except KeyboardInterrupt:
        logger.info("Session maintenance interrupted by user")
    finally:
        logger.info("Cleaning up SSH session...")
        if current_connection:
            current_connection.close()

# Legacy function for compatibility
def maintain_ssh_session(child, host, password, otp):
    """Legacy function - redirects to robust version"""
    otp_secret = extract_secret_from_url(otp) if 'otpauth://' in str(otp) else otp
    maintain_ssh_session_robust(child, host, password, otp_secret)


def main(selected_host_override=None,
         auto_monitor=True,
         monitor_refresh=DEFAULT_MONITOR_REFRESH,
         monitor_log_lines=DEFAULT_MONITOR_LOG_LINES):
    """Enhanced main function with robust connection management"""
    global should_reconnect, control_socket_path
    
    ssh_config_path = "/Users/shgao/.ssh/config"
    password_file_path = "/Users/shgao/.ssh/passwords.json"
    selected_host = None

    # Fancy colorful banner tailored for light backgrounds
    print_banner()

    logger.info("Starting Auto2FA robust login system")

    maintain_thread = None

    try:
        # 1. Read SSH config to obtain available hosts
        hosts = read_ssh_config(ssh_config_path)

        if selected_host_override:
            if selected_host_override not in hosts:
                print(style(f"❌ Host not found in SSH config: {selected_host_override}", Colors.BOLD, Colors.DANGER))
                logger.error(f"Host override not found: {selected_host_override}")
                sys.exit(1)
            selected_host = selected_host_override
            print(f"\n{style('✅ Selected (CLI):', Colors.BOLD, Colors.SUCCESS)} {style(selected_host, Colors.BOLD, Colors.ACCENT)}")
            logger.info(f"Selected host via CLI: {selected_host}")
        else:
            # 2. Show hosts to the user with fancy formatting
            print(style("📡 Available Servers:", Colors.BOLD, Colors.PRIMARY))
            print("-" * 70)
            for idx, host in enumerate(hosts, 1):
                print(f"  {style(f'[{idx}]', Colors.BOLD, Colors.WARNING)} 🖥️  {style(host, Colors.BOLD, Colors.ACCENT)}")
            print("-" * 70)

            # 3. Let user select the host
            try:
                print()
                selected_idx = int(input(f"{style('👉 Select server number:', Colors.BOLD, Colors.ACCENT)} ")) - 1
                if selected_idx < 0 or selected_idx >= len(hosts):
                    print(style("❌ Invalid selection", Colors.BOLD, Colors.DANGER))
                    sys.exit(1)
            except (ValueError, KeyboardInterrupt):
                print(f"\n{style('👋 Goodbye!', Colors.BOLD, Colors.WARNING)}")
                sys.exit(0)

            selected_host = hosts[selected_idx]
            print(f"\n{style('✅ Selected:', Colors.BOLD, Colors.SUCCESS)} {style(selected_host, Colors.BOLD, Colors.ACCENT)}")
            logger.info(f"Selected host: {selected_host}")

        # 4. Get password and OTP URL from the JSON file
        print(style("🔑 Loading credentials...", Colors.BOLD, Colors.ACCENT))
        password_config = get_password_config_for_host(selected_host, password_file_path)
        if not password_config:
            print(style(f"❌ No configuration found for {selected_host}", Colors.BOLD, Colors.DANGER))
            logger.error(f"No password configuration found for {selected_host}.")
            sys.exit(1)
        
        password = password_config.get("password")
        otpauth_url = password_config.get("otpauthUrl")

        if not password or not otpauth_url:
            print(style(f"❌ Incomplete configuration for {selected_host}", Colors.BOLD, Colors.DANGER))
            logger.error(f"Password or OTP URL not found for {selected_host}.")
            sys.exit(1)

        # 5. Extract secret from OTP URL for reuse
        otp_secret = extract_secret_from_url(otpauth_url)
        print(style("✅ 2FA secret extracted successfully", Colors.BOLD, Colors.SUCCESS))
        logger.info("OTP secret extracted successfully")

        # 6. Initial network connectivity check (non-blocking)
        print(style("🌐 Checking network...", Colors.BOLD, Colors.ACCENT))
        if not check_network_connectivity():
            print(style("⚠️  Network issues detected, will retry during connection", Colors.BOLD, Colors.WARNING))
            logger.warning("Network connectivity issues detected. Will retry during connection.")
        else:
            print(style("✅ Network OK", Colors.BOLD, Colors.SUCCESS))
            logger.info("Initial network check passed")

        # 7. Conduct initial SSH login with robust error handling
        print()
        print(style(f"🚀 Connecting to {selected_host}...", Colors.BOLD, Colors.HIGHLIGHT))
        print(style("─" * 70, Colors.MUTED))
        connection = ssh_login_with_interactive_prompt(
            selected_host,
            password,
            otp_secret,
            keep_alive=not auto_monitor
        )
        
        if connection:
            print()
            success_border = style("═" * 70, Colors.PRIMARY, Colors.BOLD)
            print(success_border)
            print(
                f"{style('✅ Successfully connected to', Colors.BOLD, Colors.SUCCESS)} "
                f"{style(selected_host, Colors.BOLD, Colors.ACCENT)}"
            )
            print(success_border)
            print(style("💡 Tips:", Colors.BOLD, Colors.PRIMARY))
            print(f"  • {style('Auto keep-alive', Colors.SUCCESS)} - no worries about disconnection")
            print(f"  • {style('Sleep/wake support', Colors.SUCCESS)} - auto-reconnect after Mac wake")
            print(f"  • {style('Network switching', Colors.SUCCESS)} - seamless reconnection")
            print(f"  • {style('Press Ctrl+C', Colors.WARNING)} to safely disconnect")
            print(success_border)
            logger.info(f"SSH login successful to {selected_host}.")
            
            if auto_monitor:
                try:
                    from monitor_ui import run_monitor
                except ImportError as exc:
                    logger.error(
                        f"无法导入监控面板所需模块: {exc}. 请运行 ./setup.sh 安装 rich 依赖。"
                    )
                    print(style("⚠️  Monitor unavailable (rich missing). Falling back to non-monitor mode.", Colors.BOLD, Colors.WARNING))
                    auto_monitor = False

            if auto_monitor:
                logger.info("Starting background session maintenance thread for monitor view")
                maintain_thread = threading.Thread(
                    target=maintain_ssh_session_robust,
                    args=(connection, selected_host, password, otp_secret),
                    kwargs={"set_signal_handlers": False},
                    daemon=True
                )
                maintain_thread.start()

                try:
                    run_monitor(selected_host, refresh=monitor_refresh, log_lines=monitor_log_lines)
                except KeyboardInterrupt:
                    print(f"\n{style('👋 Monitor interrupted by user', Colors.BOLD, Colors.WARNING)}")
                finally:
                    should_reconnect = False
                    if maintain_thread.is_alive():
                        maintain_thread.join(timeout=5)
            else:
                # Keep the main thread alive while the session is maintained
                try:
                    while should_reconnect and connection and connection.isalive():
                        time.sleep(1)
                except KeyboardInterrupt:
                    print(f"\n\n{style('👋 Disconnected by user', Colors.BOLD, Colors.WARNING)}")
                    logger.info("User requested disconnection")
                    should_reconnect = False
                
        else:
            print()
            failure_border = style("═" * 70, Colors.DANGER, Colors.BOLD)
            print(failure_border)
            print(style(f"❌ Connection failed: {selected_host}", Colors.BOLD, Colors.DANGER))
            print(failure_border)
            print(style("💡 Troubleshooting:", Colors.BOLD, Colors.PRIMARY))
            print("  • Check network connection")
            print("  • Verify SSH configuration")
            print(f"  • View logs: {style('tail -f /tmp/auto2fa.log', Colors.WARNING)}")
            print(failure_border)
            logger.error(f"SSH login failed for {selected_host} after all retry attempts.")
            sys.exit(1)
            
    except KeyboardInterrupt:
        print(f"\n\n{style('👋 Interrupted by user', Colors.BOLD, Colors.WARNING)}")
        logger.info("Program interrupted by user")
        should_reconnect = False
    except Exception as e:
        print(style(f"\n❌ Error: {e}", Colors.BOLD, Colors.DANGER))
        logger.error(f"Unexpected error in main: {e}")
        should_reconnect = False
    finally:
        # Cleanup
        should_reconnect = False
        if current_connection:
            current_connection.close()
        if selected_host:
            stop_control_master(selected_host, control_socket_path)
        closing_border = style("═" * 70, Colors.MUTED, Colors.BOLD)
        print("\n" + closing_border)
        print(style("🔒 Auto2FA session ended", Colors.BOLD, Colors.INFO))
        print(closing_border + "\n")
        logger.info("Auto2FA session ended")

# Configuration validation
def validate_configuration():
    """Validate system configuration before starting"""
    issues = []
    
    # Check if required files exist
    ssh_config_path = "/Users/shgao/.ssh/config"
    password_file_path = "/Users/shgao/.ssh/passwords.json"
    
    if not os.path.exists(ssh_config_path):
        issues.append(f"SSH config file not found: {ssh_config_path}")
    
    if not os.path.exists(password_file_path):
        issues.append(f"Password file not found: {password_file_path}")
    
    # Check if log directory is writable
    log_dir = "/tmp"
    if not os.access(log_dir, os.W_OK):
        issues.append(f"Cannot write to log directory: {log_dir}")
    
    # Check required Python modules
    required_modules = ['pexpect', 'pyotp']
    for module in required_modules:
        try:
            __import__(module)
        except ImportError:
            issues.append(f"Required module not installed: {module}")
    
    if issues:
        logger.error("Configuration validation failed:")
        for issue in issues:
            logger.error(f"  - {issue}")
        return False
    
    logger.info("Configuration validation passed")
    return True

# Startup initialization
def initialize_system():
    """Initialize system components"""
    logger.info("Initializing Auto2FA robust system...")
    
    # Validate configuration
    if not validate_configuration():
        sys.exit(1)
    
    # Setup system monitoring
    setup_system_wake_monitoring()
    
    # Initialize connection management
    global should_reconnect
    should_reconnect = True
    
    logger.info("System initialization complete")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Auto2FA - Smart SSH Login & Monitor")
    parser.add_argument("--host", help="直接指定 SSH Host，跳过交互选择")
    parser.add_argument("--no-monitor", action="store_true", help="登录后不自动启动监控面板")
    parser.add_argument("--monitor-only", action="store_true", help="仅启动监控面板，不执行登录流程")
    parser.add_argument("--monitor-refresh", type=float, default=DEFAULT_MONITOR_REFRESH, help="监控面板刷新间隔（秒）")
    parser.add_argument("--monitor-log-lines", type=int, default=DEFAULT_MONITOR_LOG_LINES, help="监控面板显示的日志行数")

    args = parser.parse_args()

    if args.monitor_only:
        if not args.host:
            parser.error("--monitor-only 需要配合 --host 使用")
        try:
            from monitor_ui import run_monitor
        except ImportError as exc:
            logger.error(f"无法导入监控面板所需模块: {exc}. 请运行 ./setup.sh 安装 rich 依赖。")
            sys.exit(1)
        run_monitor(args.host, refresh=args.monitor_refresh, log_lines=args.monitor_log_lines)
        sys.exit(0)

    try:
        initialize_system()
        main(
            selected_host_override=args.host,
            auto_monitor=not args.no_monitor,
            monitor_refresh=args.monitor_refresh,
            monitor_log_lines=args.monitor_log_lines
        )
    except Exception as e:
        logger.error(f"Fatal error: {e}")
        sys.exit(1)
