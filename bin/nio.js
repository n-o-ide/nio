#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');
const http = require('http');
const crypto = require('crypto');
const { spawn, execSync } = require('child_process');

const pkg = require('../package.json');
const VERSION = pkg.version;
const REPO = 'n-o-ide/nio';

function getPlatformInfo() {
  const platform = os.platform();
  const arch = os.arch();

  let target = '';
  let archiveName = '';
  let ext = platform === 'win32' ? '.exe' : '';

  if (platform === 'linux') {
    if (process.env.TERMUX_VERSION || (fs.existsSync('/data/data/com.termux'))) {
      target = 'aarch64-linux-android';
    } else if (arch === 'x64') {
      target = 'x86_64-unknown-linux-gnu';
    } else if (arch === 'arm64') {
      target = 'aarch64-unknown-linux-gnu';
    }
  } else if (platform === 'darwin') {
    if (arch === 'x64') {
      target = 'x86_64-apple-darwin';
    } else if (arch === 'arm64') {
      target = 'aarch64-apple-darwin';
    }
  } else if (platform === 'win32') {
    if (arch === 'x64') {
      target = 'x86_64-pc-windows-msvc';
    }
  }

  if (!target) {
    console.error(`[nio-ai] Error: Unsupported platform: ${platform} ${arch}`);
    process.exit(1);
  }

  archiveName = platform === 'win32' ? `nio-${target}.zip` : `nio-${target}.tar.gz`;

  return { platform, arch, target, archiveName, ext };
}

function fetchWithRedirects(url, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) {
      return reject(new Error('Too many redirects while downloading binary'));
    }

    const client = url.startsWith('https:') ? https : http;
    const req = client.get(url, { headers: { 'User-Agent': `nio-ai-npm/${VERSION}` } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return resolve(fetchWithRedirects(res.headers.location, maxRedirects - 1));
      }
      if (res.statusCode !== 200) {
        return reject(new Error(`Failed to download: HTTP ${res.statusCode} from ${url}`));
      }
      resolve(res);
    });

    req.on('error', reject);
  });
}

function streamToString(stream) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    stream.on('data', (d) => chunks.push(d));
    stream.on('end', () => resolve(Buffer.concat(chunks).toString('utf-8')));
    stream.on('error', reject);
  });
}

async function ensureBinary() {
  const { ext, target, archiveName, platform } = getPlatformInfo();

  // 1. Explicit override via NIO_BIN
  if (process.env.NIO_BIN) {
    if (fs.existsSync(process.env.NIO_BIN)) {
      return process.env.NIO_BIN;
    }
    console.warn(`[nio-ai] Warning: NIO_BIN set to "${process.env.NIO_BIN}" but file does not exist.`);
  }

  // 2. Local target build if running inside repo
  const localTarget = path.join(__dirname, '..', 'target', 'release', `nio${ext}`);
  if (fs.existsSync(localTarget)) {
    try {
      fs.accessSync(localTarget, fs.constants.X_OK);
      return localTarget;
    } catch {}
  }

  // 3. System PATH check
  try {
    const whichCmd = platform === 'win32' ? 'where nio' : 'which nio';
    const sysBin = execSync(whichCmd, { stdio: ['pipe', 'pipe', 'ignore'] })
      .toString()
      .trim()
      .split(/\r?\n/)[0];

    if (sysBin && fs.existsSync(sysBin)) {
      try {
        const verOutput = execSync(`"${sysBin}" --version`, { stdio: ['pipe', 'pipe', 'ignore'] }).toString();
        if (verOutput.toLowerCase().includes('nio')) {
          return sysBin;
        }
      } catch {}
    }
  } catch {}

  // 4. User cache: ~/.nio/bin/nio
  const cacheDir = path.join(os.homedir(), '.nio', 'bin');
  fs.mkdirSync(cacheDir, { recursive: true });

  const targetBinPath = path.join(cacheDir, `nio${ext}`);
  if (fs.existsSync(targetBinPath)) {
    try {
      fs.accessSync(targetBinPath, fs.constants.X_OK);
      return targetBinPath;
    } catch {
      fs.chmodSync(targetBinPath, 0o755);
      return targetBinPath;
    }
  }

  // 5. Download from GitHub release
  const tag = `v${VERSION}`;
  const baseUrl = `https://github.com/${REPO}/releases/download/${tag}`;
  const downloadUrl = `${baseUrl}/${archiveName}`;
  const sumsUrl = `${baseUrl}/SHA256SUMS`;

  console.log(`[nio-ai] Downloading NioAI binary (${tag}, ${archiveName})...`);

  // Fetch SHA256SUMS if available
  let expectedHash = null;
  try {
    const sumsRes = await fetchWithRedirects(sumsUrl);
    const sumsText = await streamToString(sumsRes);
    for (const line of sumsText.split('\n')) {
      const parts = line.trim().split(/\s+/);
      if (parts.length >= 2) {
        const hash = parts[0];
        const file = parts[1].replace(/^\*/, '');
        if (file === archiveName) {
          expectedHash = hash;
          break;
        }
      }
    }
  } catch {}

  const tempArchive = path.join(cacheDir, `.download-${Date.now()}-${archiveName}`);
  const archiveRes = await fetchWithRedirects(downloadUrl);

  const fileStream = fs.createWriteStream(tempArchive);
  const hasher = crypto.createHash('sha256');

  await new Promise((resolve, reject) => {
    archiveRes.on('data', (chunk) => {
      hasher.update(chunk);
      fileStream.write(chunk);
    });
    archiveRes.on('end', () => fileStream.end(resolve));
    archiveRes.on('error', (err) => {
      fileStream.destroy();
      fs.unlink(tempArchive, () => {});
      reject(err);
    });
  });

  const actualHash = hasher.digest('hex');
  if (expectedHash && actualHash !== expectedHash) {
    fs.unlinkSync(tempArchive);
    throw new Error(`Checksum verification failed for ${archiveName}! Expected ${expectedHash}, got ${actualHash}`);
  }

  // Extract
  const tempExtractDir = path.join(cacheDir, `.extract-${Date.now()}`);
  fs.mkdirSync(tempExtractDir, { recursive: true });

  try {
    if (archiveName.endsWith('.tar.gz')) {
      execSync(`tar -xzf "${tempArchive}" -C "${tempExtractDir}"`);
    } else if (archiveName.endsWith('.zip')) {
      if (platform === 'win32') {
        execSync(`powershell -Command "Expand-Archive -Path '${tempArchive}' -DestinationPath '${tempExtractDir}' -Force"`);
      } else {
        execSync(`unzip -q "${tempArchive}" -d "${tempExtractDir}"`);
      }
    }
  } finally {
    try { fs.unlinkSync(tempArchive); } catch {}
  }

  const extractedBin = path.join(tempExtractDir, `nio${ext}`);
  if (!fs.existsSync(extractedBin)) {
    throw new Error(`Downloaded archive did not contain nio binary`);
  }

  fs.chmodSync(extractedBin, 0o755);
  fs.renameSync(extractedBin, targetBinPath);
  try { fs.rmSync(tempExtractDir, { recursive: true, force: true }); } catch {}

  console.log(`[nio-ai] NioAI binary ready.`);
  return targetBinPath;
}

async function main() {
  try {
    const binPath = await ensureBinary();
    const args = process.argv.slice(2);

    const child = spawn(binPath, args, {
      stdio: 'inherit',
      cwd: process.cwd(),
      env: process.env
    });

    const forwardSignal = (sig) => {
      if (child.pid) {
        try { process.kill(child.pid, sig); } catch {}
      }
    };

    process.on('SIGINT', () => forwardSignal('SIGINT'));
    process.on('SIGTERM', () => forwardSignal('SIGTERM'));
    process.on('SIGHUP', () => forwardSignal('SIGHUP'));

    child.on('exit', (code) => {
      process.exit(code ?? 0);
    });

    child.on('error', (err) => {
      console.error(`[nio-ai] Execution error: ${err.message}`);
      process.exit(1);
    });
  } catch (err) {
    console.error(`[nio-ai] Error: ${err.message}`);
    process.exit(1);
  }
}

main();
