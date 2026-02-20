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
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.CoreFoundation
            darwin.apple_sdk.frameworks.SystemConfiguration
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
              (rust-bin.stable.latest.default.override {
                extensions = ["rust-src"];
              })
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              darwin.apple_sdk.frameworks.Security
              darwin.apple_sdk.frameworks.CoreFoundation
              darwin.apple_sdk.frameworks.SystemConfiguration
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
