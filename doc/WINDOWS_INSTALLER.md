Windows installer

Build the installer from the repository root with:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1
```

What the script does:

- builds `printltools` in release mode
- stages `PrintLTools.exe` under `dist\windows\staging`
- compiles `packaging\windows\PrintLTools.iss` with Inno Setup 6

Requirements:

- Rust toolchain for building the app
- Inno Setup 6 (`ISCC.exe`) installed, or pass `-InnoSetupCompiler <path>`

Installer output:

- `dist\windows\installer\PrintLTools-Setup-<version>.exe`

Silent install:

```powershell
.\PrintLTools-Setup-<version>.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
```

Useful script options:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1 -SkipBuild
powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1 -SkipCompile
```

Installer layout:

- application files go to `%ProgramFiles%\PrintLTools`
- runtime data uses `%APPDATA%\PrintLTools` as the shortcut working directory

Smart App Control

Windows 11 Smart App Control will often block an unsigned installer or an unsigned installed app binary. For release builds, sign both the packaged app executable and the generated installer with a trusted RSA code-signing certificate.

The build script supports optional signing through `signtool.exe`:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1 `
  -Sign `
  -CertificateThumbprint "<CERT_SHA1>"
```

If your certificate is in the machine store instead of the current user store:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1 `
  -Sign `
  -CertificateThumbprint "<CERT_SHA1>" `
  -CertificateStoreLocation LocalMachine
```

If you sign from a PFX file:

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-installer.ps1 `
  -Sign `
  -PfxPath "C:\path\to\codesign.pfx" `
  -PfxPassword "<PFX_PASSWORD>"
```

Optional signing arguments:

- `-SignToolPath <path>` to point at a specific `signtool.exe`
- `-CertificateSubjectName "<subject>"` to select a cert from the Windows cert store by subject name
- `-CertificateStoreLocation CurrentUser|LocalMachine` when using a store-backed certificate
- `-DigestAlgorithm SHA256|SHA384|SHA512` to control the file and timestamp digest
- `-TimestampUrl <url>` to override the RFC 3161 timestamp service (defaults to `http://timestamp.digicert.com`)

Notes:

- Smart App Control currently requires RSA-based signatures; ECC signatures are not accepted.
- Signing the installer alone is not enough if the installed app executable remains unsigned.
- Unsigned local development builds still work when `-Sign` is omitted; signing is intended for production distribution.
