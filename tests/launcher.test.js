const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { Readable } = require('stream');
const { EventEmitter } = require('events');
const vm = require('vm');
const { execFileSync, spawnSync } = require('child_process');
const { checksumFor, binaryCachePath, binaryMatchesVersion, downloadArchive, streamToString, fetchWithRedirects } = require('../bin/nio.js');

const hash = value => crypto.createHash('sha256').update(value).digest('hex');
const temporary = () => fs.mkdtempSync(path.join(os.tmpdir(), 'nio-install-test-'));
const cleanup = target => {
  try {
    fs.rmSync(target, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  } catch {}
};

test('cache separates package versions and platforms', () => {
  const first = binaryCachePath('/cache', 'linux', '', '0.3.1');
  assert.notEqual(first, binaryCachePath('/cache', 'linux', '', '0.3.2'));
  assert.notEqual(first, binaryCachePath('/cache', 'macos', '', '0.3.1'));
});

test('checksum requires one exact valid entry', () => {
  const line = `${hash('archive')}  nio-linux.tar.gz`;
  assert.equal(checksumFor(line, 'nio-linux.tar.gz'), hash('archive'));
  for (const manifest of ['', 'bad  nio-linux.tar.gz', `${line}\n${line}`, `${line}.other`]) {
    assert.throws(() => checksumFor(manifest, 'nio-linux.tar.gz'), /checksum/);
  }
});

test('downloads require HTTPS and a live deadline', async () => {
  await assert.rejects(fetchWithRedirects('http://example.invalid/file'), /HTTPS/);
  await assert.rejects(fetchWithRedirects('https://example.invalid/file', 5, Date.now() - 1), /deadline/);
});

test('archive pipeline verifies hashes and propagates stream and file errors', async () => {
  const root = temporary();
  try {
    const file = path.join(root, 'archive');
    await downloadArchive(Readable.from([Buffer.from('archive')]), file, hash('archive'));
    assert.equal(fs.readFileSync(file, 'utf8'), 'archive');
    await assert.rejects(downloadArchive(Readable.from(['wrong']), path.join(root, 'bad'), hash('archive')), /Checksum/);
    await assert.rejects(downloadArchive(Readable.from(['data']), file, hash('data')), /EEXIST/);
    const broken = new Readable({ read() { this.destroy(new Error('interrupted')); } });
    await assert.rejects(downloadArchive(broken, path.join(root, 'partial'), hash('data')), /interrupted/);
    await assert.rejects(streamToString(Readable.from([Buffer.alloc(20)]), 10), /size limit/);
  } finally { cleanup(root); }
});

test('binary identity rejects old versions and unrelated nio programs', { skip: process.platform === 'win32' }, () => {
  const root = temporary();
  try {
    const file = path.join(root, 'nio');
    for (const [output, expected] of [['nio 0.3.2 (NioAI)', true], ['nio 0.3.1 (NioAI)', false], ['nio platform 0.3.2', false]]) {
      fs.writeFileSync(file, `#!/bin/sh\nprintf '%s\\n' '${output}'\n`, { mode: 0o755 });
      assert.equal(binaryMatchesVersion(file, '0.3.2'), expected);
    }
  } finally { cleanup(root); }
});

test('launcher upgrades a stale cache, verifies before extraction, and reuses the matching version', { skip: process.platform === 'win32' }, async () => {
  const root = temporary();
  try {
    const version = require('../package.json').version;
    const contents = path.join(root, 'contents');
    fs.mkdirSync(contents);
    fs.writeFileSync(path.join(contents, 'nio'), `#!/bin/sh\necho "nio ${version} (NioAI)"\n`, { mode: 0o755 });
    const archivePath = path.join(root, 'archive.tar.gz');
    execFileSync('tar', ['-czf', archivePath, '-C', contents, 'nio']);
    const archive = fs.readFileSync(archivePath);
    const archiveName = 'nio-x86_64-unknown-linux-gnu.tar.gz';
    const downloads = [];
    let manifest = `${hash(archive)}  ${archiveName}\n`;
    const mockHttps = {
      get(url, options, callback) {
        downloads.push(url.href);
        const req = new EventEmitter();
        req.destroy = error => req.emit('error', error);
        queueMicrotask(() => {
          const body = url.pathname.endsWith('SHA256SUMS') ? Buffer.from(manifest) : archive;
          const response = Readable.from([body]);
          response.statusCode = 200;
          response.on('close', () => req.emit('close'));
          callback(response);
        });
        return req;
      }
    };
    const module = { exports: {} };
    const fakeRequire = name => {
      if (name === 'https') return mockHttps;
      if (name === 'os') return { ...os, homedir: () => root, platform: () => 'linux', arch: () => 'x64' };
      if (name === '../package.json') return { version };
      return require(name);
    };
    vm.runInNewContext(fs.readFileSync(path.resolve(__dirname, '../bin/nio.js'), 'utf8'), {
      require: fakeRequire, module, __dirname: path.join(root, 'bin'),
      process: { env: {}, platform: 'linux' }, console: { error() {} }, URL, Buffer, setTimeout, clearTimeout
    });
    const launcher = module.exports;
    const cached = binaryCachePath(root, 'x86_64-unknown-linux-gnu', '', version);
    fs.mkdirSync(path.dirname(cached), { recursive: true });
    fs.writeFileSync(cached, '#!/bin/sh\necho "nio old (NioAI)"\n', { mode: 0o755 });
    manifest = '';
    await assert.rejects(launcher.ensureBinary(), /checksum/);
    assert.match(fs.readFileSync(cached, 'utf8'), /old/);
    assert.equal(downloads.length, 1); // No archive request without a valid checksum.
    manifest = `${hash(archive)}  ${archiveName}\n`;
    assert.equal(await launcher.ensureBinary(), cached);
    assert.equal(binaryMatchesVersion(cached, version), true);
    assert.equal(downloads.length, 3);
    assert.equal(await launcher.ensureBinary(), cached);
    assert.equal(downloads.length, 3);
    assert.equal(fs.readdirSync(path.dirname(cached)).some(name => name.startsWith('.download-')), false);
  } finally { cleanup(root); }
});

test('shell installer refuses unverifiable downloads and preserves an existing executable', { skip: process.platform === 'win32' }, () => {
  const root = temporary();
  try {
    const commands = path.join(root, 'commands');
    const install = path.join(root, 'installed');
    const contents = path.join(root, 'contents');
    for (const dir of [commands, install, contents]) fs.mkdirSync(dir);
    fs.writeFileSync(path.join(contents, 'nio'), '#!/bin/sh\necho "nio 0.3.2 (NioAI)"\n', { mode: 0o755 });
    const archive = path.join(root, 'release.tar.gz');
    execFileSync('tar', ['-czf', archive, '-C', contents, 'nio']);
    const sums = path.join(root, 'sums');
    const checksum = hash(fs.readFileSync(archive));
    fs.writeFileSync(path.join(commands, 'uname'), '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n', { mode: 0o755 });
    fs.writeFileSync(path.join(commands, 'ldd'), '#!/bin/sh\necho glibc\n', { mode: 0o755 });
    fs.writeFileSync(path.join(commands, 'curl'), `#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    https:*) url="$1";;
    -o) shift; destination="$1";;
  esac
  shift
done
case "$url" in
  */SHA256SUMS) cp "$TEST_SUMS" "$destination";;
  *) cp "$TEST_ARCHIVE" "$destination";;
esac
`, { mode: 0o755 });
    const env = { ...process.env, PATH: `${commands}${path.delimiter}${process.env.PATH}`, NIO_INSTALL_DIR: install,
      NIO_VERSION: 'v0.3.2', TERMUX_VERSION: '', TEST_SUMS: sums, TEST_ARCHIVE: archive };
    const run = () => spawnSync('sh', [path.resolve(__dirname, '../install.sh')], { env, encoding: 'utf8', timeout: 10_000 });
    const destination = path.join(install, 'nio');
    const existing = '#!/bin/sh\necho "nio 0.3.1 (NioAI)"\n';
    fs.writeFileSync(destination, existing, { mode: 0o755 });
    for (const manifest of ['', `bad  nio-x86_64-unknown-linux-gnu.tar.gz`, `${'0'.repeat(64)}  nio-x86_64-unknown-linux-gnu.tar.gz`]) {
      fs.writeFileSync(sums, manifest);
      assert.notEqual(run().status, 0);
      assert.equal(fs.readFileSync(destination, 'utf8'), existing);
    }
    fs.rmSync(sums);
    assert.notEqual(run().status, 0);
    assert.equal(fs.readFileSync(destination, 'utf8'), existing);
    fs.writeFileSync(sums, `${checksum}  nio-x86_64-unknown-linux-gnu.tar.gz\n`);
    fs.writeFileSync(destination, '#!/bin/sh\necho "unrelated nio"\n');
    assert.notEqual(run().status, 0);
    assert.match(fs.readFileSync(destination, 'utf8'), /unrelated/);
    fs.writeFileSync(destination, existing);
    const result = run();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(binaryMatchesVersion(destination, '0.3.2'), true);
    assert.equal(fs.readdirSync(install).some(name => name.startsWith('.nio-install')), false);
  } finally { cleanup(root); }
});

test('PowerShell installer requires checksums before extraction and cleans up failures', { skip: process.platform !== 'win32' }, () => {
  const root = temporary();
  try {
    const install = path.join(root, 'installed');
    const temporaryDownloads = path.join(root, 'temporary');
    fs.mkdirSync(install);
    fs.mkdirSync(temporaryDownloads);
    const existing = path.join(install, 'nio.exe');
    fs.writeFileSync(existing, 'existing executable');
    const archive = path.join(root, 'archive.zip');
    fs.writeFileSync(archive, 'fixture archive');
    const checksum = hash(fs.readFileSync(archive));
    const sums = path.join(root, 'sums');
    const env = { ...process.env, TEMP: temporaryDownloads, NIO_INSTALL_DIR: install, NIO_VERSION: 'v0.3.2',
      TEST_ARCHIVE: archive, TEST_SUMS: sums, TEST_INSTALL_SCRIPT: path.resolve(__dirname, '../install.ps1') };
    const script = `
function Invoke-WebRequest {
  param($Uri, $OutFile, $TimeoutSec)
  if ($Uri.EndsWith('/SHA256SUMS')) { Copy-Item -LiteralPath $env:TEST_SUMS -Destination $OutFile }
  else { Copy-Item -LiteralPath $env:TEST_ARCHIVE -Destination $OutFile }
}
function Expand-Archive {
  param($Path, $DestinationPath, [switch]$Force)
  throw 'Verified archive reached extraction'
}
. $env:TEST_INSTALL_SCRIPT
`;
    const run = () => spawnSync('powershell', ['-NoProfile', '-NonInteractive', '-Command', script], { env, encoding: 'utf8', timeout: 20_000 });
    const name = 'nio-x86_64-pc-windows-msvc.zip';
    for (const manifest of ['', `bad  ${name}`, `${'0'.repeat(64)}  ${name}`, `${checksum}  ${name}\n${checksum}  ${name}`]) {
      fs.writeFileSync(sums, manifest);
      const result = run();
      assert.notEqual(result.status, 0);
      assert.doesNotMatch(result.stderr, /reached extraction/);
      assert.equal(fs.readFileSync(existing, 'utf8'), 'existing executable');
      assert.deepEqual(fs.readdirSync(temporaryDownloads), []);
    }
    fs.rmSync(sums);
    assert.notEqual(run().status, 0);
    fs.writeFileSync(sums, `${checksum}  ${name}\n`);
    assert.match(run().stderr, /Verified archive reached extraction/);
    assert.deepEqual(fs.readdirSync(temporaryDownloads), []);
  } finally { cleanup(root); }
});
