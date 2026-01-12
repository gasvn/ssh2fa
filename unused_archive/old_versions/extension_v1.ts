import * as vscode from 'vscode';
import { Client } from 'ssh2';
import { authenticator } from 'otplib';
import * as url from 'url';

// Function to generate OTP based on otpauth URL
function generatePasscode(otpauthUrl: string): string {
    try {
        const parsedUrl = url.parse(otpauthUrl, true);
        const secret = parsedUrl.query.secret as string;

        if (!secret) {
            throw new Error('Missing secret in otpauth URL');
        }

        const passcode = authenticator.generate(secret);
        return passcode;
    } catch (error) {
        throw new Error(`Failed to generate OTP: ${error instanceof Error ? error.message : error}`);
    }
}

// Function to perform SSH login with 2FA (password + OTP)
async function sshLogin(config: { host: string; username: string; password: string; otpauthUrl: string }) {
    console.log('Starting SSH login with config:', config);

    // Generate OTP
    let otp: string;
    try {
        console.log('Generating OTP...');
        otp = generatePasscode(config.otpauthUrl);
        console.log(`Generated OTP: ${otp}`);
    } catch (error) {
        console.error(error);
        vscode.window.showErrorMessage(`Failed to generate OTP: ${error instanceof Error ? error.message : error}`);
        return;
    }

    const conn = new Client();
    let connected = false; // Track if the connection is already established

    conn.on('debug', (info) => {
        console.log('SSH Debug:', info);
    });

    // Handle the keyboard-interactive prompt explicitly
    conn.on('keyboard-interactive', (name, instructions, lang, prompts, finish) => {
        console.log('keyboard-interactive triggered');
        const responses: string[] = [];

        prompts.forEach((prompt) => {
            console.log('Prompt:', prompt.prompt);

            // Responding to password prompt
            if (prompt.prompt.includes('Password')) {
                console.log('Sending password...');
                responses.push(config.password);  // Send the password
            }
            // Responding to OTP prompt
            else if (prompt.prompt.includes('VerificationCode')) {
                console.log('Sending OTP...');
                responses.push(otp);  // Send the OTP
            }
        });

        console.log('Sending responses:', responses);
        finish(responses);  // Finish interactive session by sending responses
    });

    conn.on('ready', () => {
        if (!connected) {  // Only proceed if not already connected
            console.log('SSH connection established');
            connected = true;

            // Keep the connection alive with a periodic "noop" command
            conn.exec('echo "Connection alive" > /dev/null', (err, stream) => {
                if (err) {
                    console.error('Error executing noop command:', err);
                    return;
                }

                // Periodically keep the session alive every 60 seconds
                setInterval(() => {
                    stream.write('echo "Connection alive" > /dev/null\n');
                    console.log('Keeping session alive...');
                }, 60000); // Send a keep-alive every 60 seconds

                stream.on('data', (data) => {
                    console.log('Output:', data.toString());
                });

                stream.on('close', () => {
                    console.log('Stream closed');
                });
            });
        }
    }).on('error', (err) => {
        console.error('SSH connection error:', err);
        // Retry connection if allowed
        console.log('Retrying SSH connection...');
        setTimeout(() => sshLogin(config), 5000);  // Retry after 5 seconds
    });

    // Attempt to connect with keyboard-interactive as the primary method
    if (!connected) {
        conn.connect({
            host: config.host,
            port: 22,
            username: config.username,
            password: config.password,  // Use password for initial authentication attempt
            tryKeyboard: true,  // Ensure keyboard-interactive is attempted
            debug: console.log,  // Add detailed debug logging
        });

        console.log('Connection attempt made...');
    }
}

// This is the main function that gets called when the user triggers the command
export function activate(context: vscode.ExtensionContext) {
    console.log('Auto 2FA SSH Login extension activated!');

    // Hardcode the configuration for debugging purposes (replace with actual values)
    const config = {
        host: 'login.rc.fas.harvard.edu', // Change to your cluster host
        username: 'shgao',                // Change to your username
        password: 'GSH@gasvn@0617',        // Change to your password
        otpauthUrl: 'otpauth://totp/shgao@login.rc.fas.harvard.edu?secret=HTQCQNLBC4LWOAIN'
    };

    console.log('Using hard-coded configuration for SSH login');
    console.log(`Starting SSH login with config: ${JSON.stringify(config)}`);

    // Call the sshLogin function to start the SSH login process
    sshLogin(config);

    // Register the command
    let disposable = vscode.commands.registerCommand('auto2fa.login', () => {
        vscode.window.showInformationMessage('Auto 2FA SSH Login extension triggered!');
        sshLogin(config);
    });

    context.subscriptions.push(disposable);
}

// This function is called when the extension is deactivated
export function deactivate() {
    console.log('Auto 2FA SSH Login extension deactivated.');
}
