# Installer Plan (`install.sh` and `install.ps1`)

## Goal

Provide a small installer that downloads a versioned, prebuilt NioAI release and installs the `nio` executable. Users should not need Rust or Cargo.

## Release artifacts

- Publish one archive per supported OS and architecture, with an explicit target name such as `nio-linux-x86_64-unknown-linux-gnu.tar.gz` or `nio-windows-x86_64-pc-windows-msvc.zip`.
- Include a `SHA256SUMS` file and verify the selected archive before extracting it.
- Publish a release manifest containing the version, target, archive name, and checksum. The installer must reject missing or mismatched checksums.
- Keep the binary name `nio`; do not replace or overwrite another executable without warning.
- Keep a separate executable/archive format for each target: tar.gz for Linux and macOS, zip for Windows.
- Build native artifacts for Linux, macOS, and Windows. Add targets only after building and running them in their intended environments.

## Platform targets to evaluate

| Environment | Candidate target | Release decision |
| --- | --- | --- |
| Standard Linux x86_64 | `x86_64-unknown-linux-gnu` | Initial target; validate on a clean Linux install. |
| Standard Linux ARM64 | `aarch64-unknown-linux-gnu` | Add after build and runtime validation. |
| Alpine Linux x86_64 / ARM64 | `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl` | Add musl builds after runtime validation on Alpine. |
| macOS Intel | `x86_64-apple-darwin` | Build and validate on an Intel Mac. |
| macOS Apple Silicon | `aarch64-apple-darwin` | Build and validate on an Apple Silicon Mac. |
| Windows x86_64 | `x86_64-pc-windows-msvc` | Build an `.exe` and validate with the PowerShell installer and shell tools. |
| Windows ARM64 | `aarch64-pc-windows-msvc` | Add after build and runtime validation; may follow x86_64 support. |
| Termux on Android ARM64 | `aarch64-linux-android` | Build as an Android binary and validate directly in Termux. Do not reuse a Linux GNU or musl binary. |
| Termux on other Android architectures | Android target matching the device | Decide supported architectures after checking demand and validating each target. |
| iSH on iOS | Candidate Linux i386/musl target; confirm against the iSH guest ABI | Experimental until a binary can be built and run successfully in iSH. Confirm architecture, ABI, and runtime compatibility on device before publishing it as supported. |

Do not infer support from a successful cross-compilation. Record the target and the environment used for each runtime check.

## Installer behavior

1. Use `install.sh` for Linux and macOS, and `install.ps1` for native Windows. Detect OS and CPU architecture before selecting a release. For Android, detect Termux explicitly (for example, `TERMUX_VERSION` or `PREFIX`) before treating it as generic Linux. WSL can use the Linux installer and Linux artifact.
2. Map the detected environment to an exact published target. If there is no match, stop with a clear message and the available targets.
3. Resolve a pinned version. Support an optional version override; avoid executing an unpinned `latest` script payload.
4. Download the matching archive and checksum manifest over HTTPS to a temporary directory.
5. Verify the archive checksum before extraction. Fail closed if verification tools or checksum data are unavailable.
6. Install `nio` into a user-writable directory already on `PATH` when possible. On Windows, use a user-local directory such as `%LOCALAPPDATA%\Programs\Nio`. Otherwise, show the chosen destination and the PATH change the user can add.
7. Preserve an existing `nio` by default. Offer a clear update path and avoid silently replacing unrelated NIO executables.
8. Verify the installed executable with `nio --version` and print a concise success message.
9. Clean up temporary files on success, failure, and interruption.

The scripts should use built-in platform tools where practical, quote paths, avoid `eval` and PowerShell expression evaluation of downloaded data, and never run downloaded content as a shell script.

The Windows target also needs native shell-command support. The current command runner invokes `sh`, which is not available by default in native Windows. Add a Windows implementation (for example, PowerShell) before claiming full Windows support; keep command approval and project-directory restrictions consistent across platforms.

## Release and validation work

- Add a release build job that builds each approved target and packages only the `nio` executable plus release metadata.
- Generate checksums from the final uploaded archives and include them in the release manifest.
- Exercise installer cases for supported targets, unsupported OS/architecture, checksum mismatch, interrupted download, existing `nio`, and PATH instructions on Linux, macOS, and Windows.
- Smoke-test file access, model requests, and approved shell commands on each supported OS, including the Windows shell implementation.
- Run a smoke check on real Termux devices for each supported Android architecture.
- Treat iSH as experimental until the installer and binary both pass an on-device smoke check. If the iSH environment cannot execute the required Rust-built binary, document that limitation rather than shipping an artifact that only compiles.

## Suggested implementation sequence

1. Confirm release hosting and the initial supported target list.
2. Set up reproducible per-target builds and versioned release archives.
3. Implement `install.sh` against the release manifest and checksum files.
4. Validate installation and updates in clean Linux, macOS, Windows, and Termux environments.
5. Investigate iSH's guest target and runtime separately; add it only after a successful device check.
6. Document installation, supported targets, upgrades, and uninstall steps in `README.md`.
