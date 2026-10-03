#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');
const crypto = require('crypto');
const { spawn, execFileSync } = require('child_process');
const { pipeline } = require('stream/promises');
const { Transform } = require('stream');

const pkg = require('../package.json');
const VERSION = pkg.version;
const REPO = 'nio-labs/nio';

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

const DOWNLOAD_TIMEOUT_MS = 120_000;
const ARCHIVE_LIMIT = 128 * 1024 * 1024;

function fetchWithRedirects(url, maxRedirects = 5, deadline = Date.now() + DOWNLOAD_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    if (parsed.protocol !== 'https:') return reject(new Error('Downloads require HTTPS'));
    const remaining = deadline - Date.now();
    if (maxRedirects < 0 || remaining <= 0) return reject(new Error('Download redirect limit or deadline exceeded'));
    const req = https.get(parsed, { headers: { 'User-Agent': `nio-ai-npm/${VERSION}` } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        try {
          resolve(fetchWithRedirects(new URL(res.headers.location, parsed).href, maxRedirects - 1, deadline));
        } catch (error) { reject(error); }
      } else if (res.statusCode !== 200) {
        res.resume();
        reject(new Error(`Failed to download: HTTP ${res.statusCode} from ${url}`));
      } else {
        resolve(res);
      }
    });
    const timer = setTimeout(() => req.destroy(new Error('Download deadline exceeded')), remaining);
    req.on('close', () => clearTimeout(timer));
    req.on('error', reject);
  });
}

async function streamToString(stream, limit = 1024 * 1024) {
  const chunks = [];
  let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (size > limit) throw new Error('Checksum manifest exceeds size limit');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

function checksumFor(manifest, archiveName) {
  const matches = manifest.split(/\r?\n/).map(line => line.trim().split(/\s+/))
    .filter(parts => parts.length === 2 && parts[1].replace(/^\*/, '') === archiveName);
  if (matches.length !== 1 || !/^[a-f0-9]{64}$/i.test(matches[0][0])) {
    throw new Error(`Missing, invalid, or duplicate checksum for ${archiveName}`);
  }
  return matches[0][0].toLowerCase();
}

function binaryMatchesVersion(binary, version = VERSION) {
  try {
    fs.accessSync(binary, fs.constants.X_OK);
    const output = execFileSync(binary, ['--version'], { encoding: 'utf8', timeout: 5000, maxBuffer: 4096, stdio: ['ignore', 'pipe', 'ignore'] });
    return output.trim() === `nio ${version} (NioAI)`;
  } catch { return false; }
}

function binaryCachePath(home, target, ext, version = VERSION) {
  return path.join(home, '.nio', 'bin', version, target, `nio${ext}`);
}

async function downloadArchive(stream, destination, expectedHash) {
  const hasher = crypto.createHash('sha256');
  let size = 0;
  const hashStream = new Transform({
    transform(chunk, encoding, callback) {
      size += chunk.length;
      if (size > ARCHIVE_LIMIT) return callback(new Error('Archive exceeds size limit'));
      hasher.update(chunk);
      callback(null, chunk);
    }
  });
  await pipeline(stream, hashStream, fs.createWriteStream(destination, { flags: 'wx', mode: 0o600 }));
  if (hasher.digest('hex') !== expectedHash) throw new Error('Checksum verification failed');
}

async function ensureBinary() {
  const { ext, target, archiveName, platform } = getPlatformInfo();
  if (process.env.NIO_BIN) {
    fs.accessSync(process.env.NIO_BIN, fs.constants.X_OK);
    return process.env.NIO_BIN;
  }
  const localTarget = path.join(__dirname, '..', 'target', 'release', `nio${ext}`);
  if (binaryMatchesVersion(localTarget)) return localTarget;

  // Do not probe nio on PATH: it may be this npm wrapper or an unrelated program.
  const targetBinPath = binaryCachePath(os.homedir(), target, ext);
  if (binaryMatchesVersion(targetBinPath)) return targetBinPath;
  const cacheDir = path.dirname(targetBinPath);
  fs.mkdirSync(cacheDir, { recursive: true });
  const tag = `v${VERSION}`;
  const baseUrl = `https://github.com/${REPO}/releases/download/${tag}`;
  console.error(`[nio-ai] Downloading NioAI binary (${tag}, ${archiveName})...`);
  const deadline = Date.now() + DOWNLOAD_TIMEOUT_MS;
  const manifest = await streamToString(await fetchWithRedirects(`${baseUrl}/SHA256SUMS`, 5, deadline));
  const expectedHash = checksumFor(manifest, archiveName);
  const temporary = fs.mkdtempSync(path.join(cacheDir, '.download-'));
  try {
    const archive = path.join(temporary, archiveName);
    await downloadArchive(await fetchWithRedirects(`${baseUrl}/${archiveName}`, 5, deadline), archive, expectedHash);
    if (archiveName.endsWith('.tar.gz')) {
      execFileSync('tar', ['-xzf', archive, '-C', temporary], { timeout: 30_000 });
    } else if (platform === 'win32') {
      const quote = value => "'" + value.replace(/'/g, "''") + "'";
      execFileSync('powershell', ['-NoProfile', '-NonInteractive', '-Command',
        `Expand-Archive -LiteralPath ${quote(archive)} -DestinationPath ${quote(temporary)} -Force`], { timeout: 30_000 });
    } else {
      execFileSync('unzip', ['-q', archive, '-d', temporary], { timeout: 30_000 });
    }
    const extracted = path.join(temporary, `nio${ext}`);
    if (!fs.lstatSync(extracted).isFile()) throw new Error('Archive did not contain a regular nio binary');
    fs.chmodSync(extracted, 0o755);
    if (!binaryMatchesVersion(extracted)) throw new Error('Downloaded binary has an unexpected version');
    fs.renameSync(extracted, targetBinPath);
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
  console.error('[nio-ai] NioAI binary ready.');
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

    child.on('exit', (code, signal) => {
      process.exit(code ?? (signal ? 128 + (os.constants.signals[signal] || 1) : 1));
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

if (require.main === module) main();
module.exports = { ensureBinary, checksumFor, binaryMatchesVersion, binaryCachePath, downloadArchive, streamToString, fetchWithRedirects };
