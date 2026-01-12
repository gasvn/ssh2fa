#!/usr/bin/env python3
"""
Auto2FA Daemon - Robust SSH Authentication System
A system daemon that maintains persistent SSH connections with 2FA for
VSCode Remote-SSH
"""

import argparse
import json
import logging
import os
import signal
import socket
import sys
import threading
import time
from datetime import datetime

# Import from existing auto2fa.py
from auto2fa import (
    extract_secret_from_url,
    check_control_master,
    start_control_master,
    stop_control_master
)


class Auto2FADaemon:
    """Main daemon class for managing SSH connections"""

    def __init__(self, config_path: str = "daemon_config.json"):
        self.config = self._load_config(config_path)
        self.running = False
        self.hosts = {}
        self.socket_server = None
        self.monitor_thread = None
        self.socket_thread = None

        # Setup logging
        self._setup_logging()

        # Load SSH hosts
        self._load_ssh_hosts()

        # Setup signal handlers
        signal.signal(signal.SIGTERM, self._signal_handler)
        signal.signal(signal.SIGINT, self._signal_handler)

        self.logger.info("Auto2FA Daemon initialized")

    def _load_config(self, config_path: str) -> dict:
        """Load daemon configuration"""
        try:
            with open(config_path, 'r') as f:
                return json.load(f)
        except Exception as e:
            print(f"Error loading config: {e}")
            sys.exit(1)

    def _setup_logging(self):
        """Setup logging configuration"""
        log_file = self.config['daemon']['log_file']
        log_level = getattr(logging, self.config['monitoring']['log_level'])

        # Create formatter
        formatter = logging.Formatter(
            '%(asctime)s - %(name)s - %(levelname)s - %(message)s'
        )

        # Setup file handler
        file_handler = logging.FileHandler(log_file)
        file_handler.setFormatter(formatter)

        # Setup console handler
        console_handler = logging.StreamHandler()
        console_handler.setFormatter(formatter)

        # Setup logger
        self.logger = logging.getLogger('auto2fa_daemon')
        self.logger.setLevel(log_level)
        self.logger.addHandler(file_handler)
        self.logger.addHandler(console_handler)

    def _load_ssh_hosts(self):
        """Load SSH hosts from config file"""
        ssh_config_path = self.config['ssh']['config_path']
        passwords_path = self.config['ssh']['passwords_path']

        try:
            # Read SSH config
            with open(ssh_config_path, 'r') as f:
                lines = f.readlines()
                for line in lines:
                    line = line.strip()
                    if (line.startswith("Host ") and
                            not line.startswith("Host *")):
                        host = line.split()[1]
                        self.hosts[host] = {
                            'name': host,
                            'connected': False,
                            'last_check': None,
                            'control_socket': None,
                            'password': None,
                            'otp_secret': None,
                            'retry_count': 0,
                            'last_error': None
                        }

            # Load passwords
            with open(passwords_path, 'r') as f:
                passwords = json.load(f)
                for host_name, host_data in self.hosts.items():
                    if host_name in passwords:
                        host_data['password'] = passwords[host_name].get(
                            'password'
                        )
                        otp_url = passwords[host_name].get('otpauthUrl')
                        if otp_url:
                            host_data['otp_secret'] = extract_secret_from_url(
                                otp_url
                            )

            self.logger.info(
                f"Loaded {len(self.hosts)} hosts from configuration"
            )

        except Exception as e:
            self.logger.error(f"Error loading SSH hosts: {e}")
            sys.exit(1)

    def _signal_handler(self, signum, frame):
        """Handle shutdown signals"""
        self.logger.info(f"Received signal {signum}, shutting down...")
        self.running = False
        self._cleanup()
        sys.exit(0)

    def _cleanup(self):
        """Cleanup resources on shutdown"""
        self.logger.info("Cleaning up resources...")

        # Stop all connections
        for host_data in self.hosts.values():
            if host_data['control_socket']:
                stop_control_master(
                    host_data['name'], host_data['control_socket']
                )

        # Close socket server
        if self.socket_server:
            self.socket_server.close()

        # Remove socket file
        socket_path = self.config['daemon']['socket_path']
        if os.path.exists(socket_path):
            os.unlink(socket_path)

        self.logger.info("Cleanup completed")

    def _check_host_connection(self, host_name: str) -> bool:
        """Check if a host connection is still active"""
        host_data = self.hosts[host_name]

        if not host_data['control_socket']:
            return False

        return check_control_master(
            host_name, host_data['control_socket']
        )

    def _reconnect_host(self, host_name: str) -> bool:
        """Reconnect to a specific host"""
        host_data = self.hosts[host_name]

        if not host_data['password'] or not host_data['otp_secret']:
            self.logger.error(f"Missing credentials for {host_name}")
            return False

        try:
            # Stop existing connection
            if host_data['control_socket']:
                stop_control_master(
                    host_name, host_data['control_socket']
                )

            # Start new control master
            socket_path = start_control_master(
                host_name,
                host_data['password'],
                host_data['otp_secret']
            )

            host_data['control_socket'] = socket_path
            host_data['connected'] = True
            host_data['last_check'] = datetime.now()
            host_data['retry_count'] = 0
            host_data['last_error'] = None

            self.logger.info(f"Successfully reconnected to {host_name}")
            return True

        except Exception as e:
            host_data['connected'] = False
            host_data['last_error'] = str(e)
            host_data['retry_count'] += 1
            self.logger.error(
                f"Failed to reconnect to {host_name}: {e}"
            )
            return False

    def _monitor_connections(self):
        """Monitor all host connections and reconnect if needed"""
        check_interval = self.config['daemon']['check_interval']
        max_retries = self.config['daemon']['max_retries']

        while self.running:
            try:
                for host_name, host_data in self.hosts.items():
                    if not self.running:
                        break

                    # Check connection status
                    is_connected = self._check_host_connection(host_name)
                    host_data['last_check'] = datetime.now()

                    if is_connected:
                        if not host_data['connected']:
                            self.logger.info(
                                f"Connection restored for {host_name}"
                            )
                        host_data['connected'] = True
                        host_data['retry_count'] = 0
                        host_data['last_error'] = None
                    else:
                        if host_data['connected']:
                            self.logger.warning(
                                f"Connection lost for {host_name}"
                            )
                        host_data['connected'] = False

                        # Attempt reconnection if retry count is below limit
                        if host_data['retry_count'] < max_retries:
                            self.logger.info(
                                f"Attempting to reconnect {host_name} "
                                f"(attempt {host_data['retry_count'] + 1})"
                            )
                            if self._reconnect_host(host_name):
                                self.logger.info(
                                    f"Successfully reconnected to {host_name}"
                                )
                            else:
                                self.logger.warning(
                                    f"Reconnection failed for {host_name}"
                                )
                        else:
                            self.logger.error(
                                f"Max retries exceeded for {host_name}, "
                                f"giving up"
                            )

                time.sleep(check_interval)

            except Exception as e:
                self.logger.error(f"Error in connection monitoring: {e}")
                time.sleep(check_interval)

    def _handle_socket_request(self, client_socket, address):
        """Handle incoming socket requests"""
        try:
            data = client_socket.recv(1024).decode('utf-8')
            if not data:
                return

            request = json.loads(data)
            command = request.get('command')

            if command == 'status':
                response = {
                    'hosts': {
                        name: {
                            'connected': data['connected'],
                            'last_check': (
                                data['last_check'].isoformat()
                                if data['last_check'] else None
                            ),
                            'retry_count': data['retry_count'],
                            'last_error': data['last_error']
                        }
                        for name, data in self.hosts.items()
                    }
                }
            elif command == 'reconnect':
                host_name = request.get('host')
                if host_name in self.hosts:
                    success = self._reconnect_host(host_name)
                    response = {'success': success, 'host': host_name}
                else:
                    response = {
                        'success': False,
                        'error': f'Host {host_name} not found'
                    }
            elif command == 'restart':
                # Restart all connections
                for host_name in self.hosts:
                    self._reconnect_host(host_name)
                response = {
                    'success': True,
                    'message': 'All connections restarted'
                }
            else:
                response = {'error': f'Unknown command: {command}'}

            client_socket.send(
                json.dumps(response).encode('utf-8')
            )

        except Exception as e:
            self.logger.error(f"Error handling socket request: {e}")
            error_response = {'error': str(e)}
            client_socket.send(
                json.dumps(error_response).encode('utf-8')
            )
        finally:
            client_socket.close()

    def _socket_server(self):
        """Run the Unix socket server"""
        socket_path = self.config['daemon']['socket_path']

        # Remove existing socket file
        if os.path.exists(socket_path):
            os.unlink(socket_path)

        self.socket_server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket_server.bind(socket_path)
        self.socket_server.listen(5)

        # Set socket permissions
        os.chmod(socket_path, 0o666)

        self.logger.info(f"Socket server listening on {socket_path}")

        while self.running:
            try:
                client_socket, address = self.socket_server.accept()
                thread = threading.Thread(
                    target=self._handle_socket_request,
                    args=(client_socket, address)
                )
                thread.daemon = True
                thread.start()
            except Exception as e:
                if self.running:
                    self.logger.error(f"Socket server error: {e}")
                break

    def start(self):
        """Start the daemon"""
        self.logger.info("Starting Auto2FA Daemon...")
        self.running = True

        # Start initial connections
        for host_name in self.hosts:
            self.logger.info(
                f"Establishing initial connection to {host_name}"
            )
            self._reconnect_host(host_name)

        # Start monitoring thread
        self.monitor_thread = threading.Thread(
            target=self._monitor_connections
        )
        self.monitor_thread.daemon = True
        self.monitor_thread.start()

        # Start socket server thread
        self.socket_thread = threading.Thread(target=self._socket_server)
        self.socket_thread.daemon = True
        self.socket_thread.start()

        self.logger.info("Auto2FA Daemon started successfully")

        # Keep main thread alive
        try:
            while self.running:
                time.sleep(1)
        except KeyboardInterrupt:
            self.logger.info("Received keyboard interrupt")
            self.running = False

    def stop(self):
        """Stop the daemon"""
        self.logger.info("Stopping Auto2FA Daemon...")
        self.running = False
        self._cleanup()


def main():
    """Main entry point"""
    parser = argparse.ArgumentParser(description="Auto2FA Daemon")
    parser.add_argument(
        "--config", default="daemon_config.json",
        help="Configuration file path"
    )
    parser.add_argument(
        "--daemon", action="store_true", help="Run as daemon"
    )
    parser.add_argument(
        "--stop", action="store_true", help="Stop running daemon"
    )

    args = parser.parse_args()

    if args.stop:
        # Stop daemon by sending SIGTERM to PID file
        pid_file = "/tmp/auto2fa_daemon.pid"
        if os.path.exists(pid_file):
            with open(pid_file, 'r') as f:
                pid = int(f.read().strip())
            os.kill(pid, signal.SIGTERM)
            print("Daemon stop signal sent")
        else:
            print("Daemon PID file not found")
        return

    # Create daemon instance
    daemon = Auto2FADaemon(args.config)

    if args.daemon:
        # Write PID file
        with open("/tmp/auto2fa_daemon.pid", 'w') as f:
            f.write(str(os.getpid()))

        # Redirect stdout/stderr to log file
        log_file = daemon.config['daemon']['log_file']
        with open(log_file, 'a') as f:
            sys.stdout = f
            sys.stderr = f

        daemon.start()
    else:
        # Run in foreground
        daemon.start()


if __name__ == "__main__":
    main()

