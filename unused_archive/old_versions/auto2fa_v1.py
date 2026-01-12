import json
import pyotp
import urllib.parse
from pathlib import Path
import os
import sys
import select
import pexpect
import time


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
        print(f"Decoded OTP URL: {decoded_url}")
        
        # Extract the secret from the URL (after ?secret=)
        if "secret=" not in decoded_url:
            raise ValueError("The OTP URL does not contain a 'secret' parameter.")
        
        secret = decoded_url.split("secret=")[1]
        print(f"Extracted Secret: {secret}")
        
        # Generate the OTP
        otp = pyotp.TOTP(secret)
        return otp.now()
    except Exception as e:
        print(f"Failed to generate OTP: {e}")
        raise

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

# Function to perform the SSH login (keyboard-interactive)
def ssh_login_with_interactive_prompt(host, password, otp, retries=3, keep_alive=True):
    attempt = 0
    child = None
    
    while attempt < retries:
        try:
            # Construct the ssh command with -t to force pseudo-terminal allocation
            cmd = f"ssh -t -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null {host}"
            print(f"Attempting to login to {host}...")

            # Start the SSH session with pexpect
            child = pexpect.spawn(cmd, encoding='utf-8')

            # Expect the password prompt
            index = child.expect([r"Password:", r"VerificationCode:", pexpect.TIMEOUT, pexpect.EOF], timeout=30)

            # Handle the Password prompt
            if index == 0:
                print("Password prompt detected.")
                child.sendline(password)  # Send password
                index = child.expect([r"Password:", r"VerificationCode:", pexpect.TIMEOUT, pexpect.EOF], timeout=30)

            # Handle the VerificationCode prompt
            if index == 1:
                print("VerificationCode prompt detected.")
                child.sendline(otp)  # Send OTP passcode

            # Check if we're logged in or need to retry
            if index == pexpect.TIMEOUT:
                print(f"SSH connection timeout while logging into {host}. Retrying...")
                attempt += 1
            elif index == pexpect.EOF:
                print(f"Connection closed unexpectedly. Reconnecting...")
                attempt += 1
            else:
                print(f"Login successful for {host}.")
                if keep_alive:
                    return maintain_ssh_session(child)  # Keep the session alive

            # Give time for SSH to process the input
            time.sleep(1)

        except Exception as e:
            print(f"SSH login error: {e}")
            attempt += 1
            time.sleep(2)  # Delay before retrying

    print(f"Failed to login after {retries} attempts.")
    return False  # Return False if all retries failed

def maintain_ssh_session(child):
    """Maintain the SSH session and handle any disconnections."""
    print("Maintaining SSH session...")
    try:
        while True:
            # You can either send a keep-alive command like 'echo' or just wait for user input
            child.sendline("echo 'Keeping SSH connection alive...'")
            index = child.expect([pexpect.TIMEOUT, pexpect.EOF], timeout=60)  # Wait for response
            # Check if the session has been closed
            if index == pexpect.EOF:
                print("SSH session closed. Reconnecting...")
                break  # Exit the loop and reconnect

            elif index == pexpect.TIMEOUT:
                print("SSH session is still alive...")

            # Sleep for a bit before sending the next "keep-alive" message
            time.sleep(2)  # Adjust this interval if necessary

    except Exception as e:
        print(f"Error while maintaining SSH session: {e}")
        return False  # Return False if maintaining the session fails


def main():
    ssh_config_path = "/Users/shgao/.ssh/config"
    password_file_path = "/Users/shgao/.ssh/passwords.json"  # Path to your JSON file with passwords

    # 1. Read SSH config to obtain available hosts
    hosts = read_ssh_config(ssh_config_path)

    # 2. Show hosts to the user
    print("Available hosts:")
    for idx, host in enumerate(hosts, 1):
        print(f"{idx}. {host}")

    # 3. Let user select the host
    selected_idx = int(input("Select the number of the host you want to login to: ")) - 1
    if selected_idx < 0 or selected_idx >= len(hosts):
        print("Invalid selection.")
        sys.exit(1)

    selected_host = hosts[selected_idx]
    print(f"Selected host: {selected_host}")

    # 4. Get password and OTP URL from the JSON file
    password_config = get_password_config_for_host(selected_host, password_file_path)
    if not password_config:
        print(f"No password configuration found for {selected_host}.")
        sys.exit(1)
    
    password = password_config.get("password")
    otpauth_url = password_config.get("otpauthUrl")

    if not password or not otpauth_url:
        print(f"Password or OTP URL not found for {selected_host}.")
        sys.exit(1)

    # 5. Generate passcode from OTP URL
    otp = generate_passcode(otpauth_url)

    # 6. Conduct SSH login (keyboard-interactive)
    if ssh_login_with_interactive_prompt(selected_host, password, otp):
        print(f"SSH login successful to {selected_host}.")
    else:
        print(f"SSH login failed for {selected_host}.")

if __name__ == "__main__":
    main()