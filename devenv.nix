{
  pkgs,
  ...
}:
{
  packages = with pkgs; [
    actionlint
    cargo-audit
    cargo-deny
    cargo-llvm-cov
    cargo-nextest
    git
    go-task
    nixfmt
    nodejs_24
    sqlite
    taplo
  ];

  languages.rust = {
    enable = true;
    toolchainFile = ./rust-toolchain.toml;
  };

  scripts = {
    coterie-rustfmt.exec = "cargo fmt --all -- --check";
    coterie-clippy.exec = "cargo clippy --workspace --all-targets --all-features -- -D warnings";
    coterie-taplo.exec = ''
      git ls-files --cached --others --exclude-standard -z '*.toml' \
        | xargs -0 --no-run-if-empty taplo fmt --check
    '';
    coterie-nixfmt.exec = ''
      git ls-files --cached --others --exclude-standard -z '*.nix' \
        | xargs -0 --no-run-if-empty nixfmt --check
    '';
  };

  git-hooks.hooks = {
    coterie-rustfmt = {
      enable = true;
      name = "rustfmt";
      entry = "coterie-rustfmt";
      pass_filenames = false;
    };

    coterie-clippy = {
      enable = true;
      name = "clippy";
      entry = "coterie-clippy";
      pass_filenames = false;
    };

    coterie-taplo = {
      enable = true;
      name = "taplo";
      entry = "coterie-taplo";
      pass_filenames = false;
    };

    coterie-nixfmt = {
      enable = true;
      name = "nixfmt";
      entry = "coterie-nixfmt";
      pass_filenames = false;
    };
  };

  enterTest = ''
    task check
  '';
}
