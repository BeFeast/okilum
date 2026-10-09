# Building and opening Okilum

Build the platform-native executable from an identified checkout. These commands
stage development binaries; they do not install launchers, services or providers.
The [README](../README.md#build-and-open) describes ordinary local-folder use.
The optional historical Brain backend is not required to build or open the Reader.

## Source and Rust

Use your normal Git authentication. For a candidate-specific check, select the
exact source commit from that candidate's provenance before building. Otherwise
this starts from the repository's default branch:

```sh
git clone https://git.oklabs.uk/BeFeast/okilum.git
cd okilum
git rev-parse HEAD
```

The verified Linux builds use Rust 1.96.1. The repository currently has no
`rust-toolchain` file, so select the version explicitly; `Cargo.lock` pins package
resolution. If rustup is missing, install it using the [official rustup instructions](https://rustup.rs/).

```sh
rustup toolchain install 1.96.1 --profile minimal
rustc +1.96.1 --version
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
```

The vendor script clones the pinned `gpui-kit` revision and applies this checkout's
patches. A fresh upstream clone is unpatched. Verification must succeed before
using a build as evidence; it checks both the revision and every patch. Preserve
`--locked` in the build commands below. There is no need to regenerate branding:
Noto Sans, Cascadia Code and the app symbol are embedded in the source assets.

## Linux

Linux x86_64 has native build/run evidence. Arch-based Linux is the primary target.
The shell needs a native C/C++ toolchain, CMake, pkg-config and the following
window/font libraries (Debian/Ubuntu package names shown):

```sh
sudo apt install build-essential cmake pkg-config \
    libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libfontconfig-dev libfreetype-dev libxcb1-dev
```

On Arch, the corresponding packages are `base-devel`, `cmake`, `pkgconf`,
`wayland`, `libxkbcommon`, `libxkbcommon-x11`, `fontconfig`, `freetype2` and `libxcb`.
The `xkbcommon-x11` library is required at the final link, even when the Wayland
libraries are already present. These are Linux prerequisites, not Mac prerequisites.
Embedded fonts remove the need to install font families separately; fontconfig
and FreeType are still platform libraries.

```sh
CC=/usr/bin/cc CXX=/usr/bin/c++ \
cargo +1.96.1 build --release --locked -p okilum-shell -p okilum-cored
```

The executables are `target/release/okilum` and `target/release/okilum-cored`.
Run them in the appropriate role; opening the GUI does not start or replace an
existing Brain backend.

## macOS Apple Silicon

Install Xcode command-line tools and Rust with the aarch64-apple-darwin target.
Check the selected SDK and compiler:

```sh
xcrun --sdk macosx --show-sdk-path
xcrun --find clang
rustup target add --toolchain 1.96.1 aarch64-apple-darwin
```

Release signing/notarization runs on the project's configured Forgejo macOS
runner. A manual source build does not require the project's signing identity.

After the source/vendor steps above, fetch the pinned Sparkle framework that the
build links against, then build the desktop client on the Mac:

```sh
python3 scripts/updater/sparkle.py fetch /tmp/Sparkle-2.10.0.tar.xz
python3 scripts/updater/sparkle.py prepare --archive /tmp/Sparkle-2.10.0.tar.xz --destination vendor/sparkle
CC=/usr/bin/clang \
CXX=/usr/bin/clang++ \
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)" \
CARGO_TARGET_DIR="$PWD/target-macos" \
cargo +1.96.1 build --release --locked \
  --target aarch64-apple-darwin -p okilum-shell
```

This builds the GUI client; an existing remote Brain backend remains separate.
The executable is `target-macos/aarch64-apple-darwin/release/okilum`; run it with
`DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle"` because it sits outside an app bundle. No Linux
libraries or separate font installation are needed. If a dependency is missing,
check the platform prerequisites and retain the exact build error.

The pinned GPUI Apple build script uses `runtime_shaders`, compiling Metal shaders at runtime;
it does not establish a separate build-time MetalToolchain download prerequisite.
That is source inspection, not a verified Metal launch on M4.

For a first read-only smoke, use disposable files and a separate index:

```sh
OKILUM_SMOKE_DIR="$(mktemp -d)"
mkdir "$OKILUM_SMOKE_DIR/vault"
printf '# Okilum on M4\n\nMarkdown and typography.\n' \
  > "$OKILUM_SMOKE_DIR/vault/README.md"
./target-macos/aarch64-apple-darwin/release/okilum \
  --vault "$OKILUM_SMOKE_DIR/vault" \
  --index-dir "$OKILUM_SMOKE_DIR/index"
```

Inspect rendering, search, selection/copy and Appearance. Record the actual source
commit, OS, build result and observed behavior. This Reader check does not verify
Brain source writes, durable journals, native dialogs or crash recovery. Those
remain unverified on macOS until checked on the Mac. Opening a Brain additionally
requires its existing profile/verified workspace identity and reachable loopback
endpoint (through an existing tunnel for a remote backend); this build does not
configure either or transfer provider credentials.

## Windows diagnostic build

See [Windows diagnostic builds](windows-diagnostic.md). This is a portable
Reader preview, not a supported installer or an auto-update channel.

## Release configuration (maintainers)

Forgejo remains the only CI/publisher. Configure repository or organization vars:
`OKILUM_SIGNING_IDENTITY`, `OKILUM_NOTARY_PROFILE`, `R2_ENDPOINT`, and optionally
`FORGEJO_CHECKOUT_URL` for runner-specific network routing. The default checkout
and API URL is the public Forgejo server. Never put private keys in these vars;
signing/upload credentials remain in Forgejo secrets or the macOS keychain.
The public Sparkle verification key is intentionally public.

All platform packaging scripts include `LICENSE` and `THIRD_PARTY_NOTICES.md`.
Regenerate notices after dependency/asset changes; see [license sources](../licenses/README.md).
