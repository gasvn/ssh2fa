import { authenticator } from 'otplib';
import * as url from 'url';

function generatePasscode(otpauthUrl: string): string {
  // 解析 OTPAUTH URL，获取 secret
  const parsedUrl = url.parse(otpauthUrl, true);
  const secret = parsedUrl.query.secret as string;

  if (!secret) {
    throw new Error('Missing secret in otpauth URL');
  }

  // 使用 authenticator 来生成 passcode
  const passcode = authenticator.generate(secret);

  return passcode;
}

// 示例使用
const otpauthUrl = 'otpauth://totp/shgao@login.rc.fas.harvard.edu?secret=HTQCQNLBC4LWOAIN';
const passcode = generatePasscode(otpauthUrl);
console.log(`Generated Passcode: ${passcode}`);
