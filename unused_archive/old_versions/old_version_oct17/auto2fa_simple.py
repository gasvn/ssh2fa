#!/usr/bin/env python3
"""
Auto2FA Simple Version - For debugging and quick connections
Removes aggressive network checking that might interfere with SSH connections
"""

import json
import pyotp
import urllib.parse
import os
import sys
import pexpect
import time
import logging
from datetime import datetime

# Simple logging setup
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s',
    handlers=[
        logging.FileHandler('/tmp/auto2fa_simple.log'),
        logging.StreamHandler()
    ]
)
logger = logging.getLogger(__name__)

def get_password_config_for_host(host, password_file_path):
    try:
        with open(password_file_path, 'r') as f:
            passwords = json.load(f)
            return passwords.get(host)
    except Exception as e:
        logger.error(f"Error reading password config file: {e}")
        sys.exit(1)

def generate_passcode(otpauth_url):
    try:
        decoded_url = urllib.parse.unquote(otpauth_url)
        logger.info(f"Processing OTP URL")
        
        if "secret=" not in decoded_url:
            raise ValueError("The OTP URL does not contain a 'secret' parameter.")
        
        secret = decoded_url.split("secret=")[1].split('&')[0]
        logger.info(f"Secret extracted successfully")
        
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
        logger.error(f"Failed to extract secret from URL: {e}")
        raise

def generate_passcode_from_secret(secret):
    try:
        otp = pyotp.TOTP(secret)
        return otp.now()
    except Exception as e:
        logger.error(f"Failed to generate OTP from secret: {e}")
        raise

def read_ssh_config(ssh_config_path):
    try:
        hosts = []
        with open(ssh_config_path, 'r') as f:
            lines = f.readlines()
            for line in lines:
                line = line.strip()
                if line.startswith("Host "):
                    host = line.split()[1]
                    if host != "*":  # Skip wildcard entries
                        hosts.append(host)
        return hosts
    except Exception as e:
        logger.error(f"Error reading SSH config: {e}")
        sys.exit(1)

def ssh_login_simple(host, password, otp_secret, retries=3):
    """Simplified SSH login without aggressive network checking"""
    
    for attempt in range(retries):
        try:
            # Generate fresh OTP for each attempt
            fresh_otp = generate_passcode_from_secret(otp_secret)
            
            # Simple SSH command
            cmd = f"ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null {host}"
            
            logger.info(f"Attempting to login to {host} (attempt {attempt + 1}/{retries})...")

            # Start SSH session
            child = pexpect.spawn(cmd, encoding='utf-8', timeout=30)

            # Wait for prompts
            index = child.expect([
                r"[Pp]assword:",
                r"[Vv]erification[Cc]ode:",
                r"[Tt]oken:",
                r"\$",
                r"#",
                pexpect.TIMEOUT,
                pexpect.EOF
            ], timeout=30)

            # Handle password prompt
            if index == 0:
                logger.info("Password prompt detected")
                child.sendline(password)
                index = child.expect([
                    r"[Vv]erification[Cc]ode:",
                    r"[Tt]oken:",
                    r"\$",
                    r"#",
                    pexpect.TIMEOUT,
                    pexpect.EOF
                ], timeout=30)

            # Handle OTP prompt
            if index == 0 or index == 1:
                logger.info("OTP prompt detected")
                child.sendline(fresh_otp)
                index = child.expect([
                    r"\$",
                    r"#",
                    pexpect.TIMEOUT,
                    pexpect.EOF
                ], timeout=30)

            # Check for success
            if index == 0 or index == 1:
                logger.info(f"Login successful for {host}")
                
                # Keep session alive with simple method
                try:
                    print(f"Connected to {host}. Press Ctrl+C to disconnect.")
                    while True:
                        child.sendline("echo keepalive")
                        child.expect([r"keepalive", pexpect.TIMEOUT], timeout=60)
                        time.sleep(30)
                except KeyboardInterrupt:
                    logger.info("User requested disconnection")
                    child.close()
                    return True
                
            elif index == 2:  # TIMEOUT
                logger.warning(f"Connection timeout for {host}")
                child.close()
                
            else:  # EOF
                logger.warning(f"Connection closed for {host}")
                child.close()

        except Exception as e:
            logger.error(f"SSH error: {e}")
            
        # Wait before retry
        if attempt < retries - 1:
            wait_time = (attempt + 1) * 2
            logger.info(f"Waiting {wait_time} seconds before retry...")
            time.sleep(wait_time)

    logger.error(f"Failed to login to {host} after {retries} attempts")
    return False

def main():
    ssh_config_path = "/Users/shgao/.ssh/config"
    password_file_path = "/Users/shgao/.ssh/passwords.json"

    logger.info("Starting Auto2FA Simple Version")

    try:
        # Read SSH config
        hosts = read_ssh_config(ssh_config_path)

        # Show hosts
        print("Available hosts:")
        for idx, host in enumerate(hosts, 1):
            print(f"{idx}. {host}")

        # Get user selection
        try:
            selected_idx = int(input("Select host number: ")) - 1
            if selected_idx < 0 or selected_idx >= len(hosts):
                print("Invalid selection.")
                sys.exit(1)
        except (ValueError, KeyboardInterrupt):
            print("\nExiting...")
            sys.exit(0)

        selected_host = hosts[selected_idx]
        logger.info(f"Selected host: {selected_host}")

        # Get credentials
        password_config = get_password_config_for_host(selected_host, password_file_path)
        if not password_config:
            logger.error(f"No configuration found for {selected_host}")
            sys.exit(1)
        
        password = password_config.get("password")
        otpauth_url = password_config.get("otpauthUrl")

        if not password or not otpauth_url:
            logger.error(f"Missing password or OTP URL for {selected_host}")
            sys.exit(1)

        # Extract secret
        otp_secret = extract_secret_from_url(otpauth_url)
        
        # Attempt login
        if ssh_login_simple(selected_host, password, otp_secret):
            logger.info("Session completed successfully")
        else:
            logger.error("Login failed")
            sys.exit(1)
            
    except KeyboardInterrupt:
        logger.info("Program interrupted by user")
    except Exception as e:
        logger.error(f"Unexpected error: {e}")
        sys.exit(1)

if __name__ == "__main__":
    main()