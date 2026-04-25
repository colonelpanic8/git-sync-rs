{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        cargoManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        packageVersion = cargoManifest.package.version;
        gitCommitHash =
          if self ? shortRev
          then self.shortRev
          else if self ? dirtyShortRev
          then self.dirtyShortRev
          else if self ? rev
          then builtins.substring 0 12 self.rev
          else if self ? dirtyRev
          then builtins.substring 0 12 self.dirtyRev
          else "unknown";
        commonBuildInputs = with pkgs;
          [
            openssl
            libgit2
          ];

        commonEnv = {
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          OPENSSL_DIR = "${pkgs.openssl.dev}";
          OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
          OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
        };

        mkGitSyncPackage = { pname, buildFeatures ? [], extraBuildInputs ? [], extraNativeBuildInputs ? [] }:
          pkgs.rustPlatform.buildRustPackage ({
            inherit pname;
            version = packageVersion;
            GIT_COMMIT_HASH = gitCommitHash;

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            inherit buildFeatures;

            nativeBuildInputs = with pkgs; [
              pkg-config
              git
            ] ++ extraNativeBuildInputs;

            buildInputs = commonBuildInputs ++ extraBuildInputs;

            postInstall = ''
              cat > "$out/bin/git-sync" <<'EOF'
              #!${pkgs.runtimeShell}
              set -eu

              translated_args=()

              while [ "$#" -gt 0 ]; do
                case "$1" in
                  -s|--sync)
                    shift
                    ;;
                  -n|--new-files)
                    translated_args+=(--new-files=true)
                    shift
                    ;;
                  *)
                    translated_args+=("$1")
                    shift
                    ;;
                esac
              done

              exec "$(dirname "$0")/git-sync-rs" "''${translated_args[@]}" sync
              EOF
              chmod +x "$out/bin/git-sync"

              cat > "$out/bin/git-sync-on-inotify" <<'EOF'
              #!${pkgs.runtimeShell}
              set -eu

              exec "$(dirname "$0")/git-sync-rs" "$@" watch
              EOF
              chmod +x "$out/bin/git-sync-on-inotify"
            '';
          } // commonEnv);
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs;
            [
              # System dependencies for git2/openssl
              pkg-config
              openssl
              git
              libgit2
              dbus

              # Rust toolchain
              (rust-bin.stable.latest.minimal.override {
                extensions = [
                  "clippy"
                  "rust-src"
                  "rustfmt"
                ];
              })
            ];

          # Environment variables for OpenSSL
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          OPENSSL_DIR = "${pkgs.openssl.dev}";
          OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
          OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";

          # Rust debugging
          RUST_BACKTRACE = 1;
          RUST_LOG = "git_sync_rs=debug";

          shellHook = ''
            echo "git-sync-rs development environment"
            echo "Rust version: $(rustc --version)"
            echo "Cargo version: $(cargo --version)"
          '';
        };

        packages.default = mkGitSyncPackage {
          pname = "git-sync-rs";
          buildFeatures = ["tray"];
          extraBuildInputs = with pkgs; [dbus];
        };

        packages.git-sync-rs = mkGitSyncPackage {
          pname = "git-sync-rs";
        };

        packages.git-sync-rs-tray = mkGitSyncPackage {
          pname = "git-sync-rs";
          buildFeatures = ["tray"];
          extraBuildInputs = with pkgs; [dbus];
        };
      }
    );
}
