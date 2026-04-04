# Raspberry Pi Install

Local checkout install:

```bash
./rpi/install.sh
```

Remote self-deploy install after the GitHub repo exists:

```bash
curl -fsSL https://raw.githubusercontent.com/nbeathoven/emmc-lab/main/rpi/install.sh | bash
```

Behavior:

- checks required Debian packages and skips any already installed
- installs missing required packages with `apt`
- installs optional `mmc-utils` and `fio` by default
- installs Rust with `rustup` if `cargo` is missing
- clones or updates the GitHub checkout when not run inside the repo
- builds `emmc-lab` in release mode
- installs `emmc-lab` into `/usr/local/bin`

Optional environment variables:

- `EMMC_LAB_INSTALL_OPTIONAL=0` to skip optional tools
- `EMMC_LAB_PREFIX=/custom/prefix`
- `EMMC_LAB_SRC_DIR=/custom/src/path`
- `EMMC_LAB_REPO_URL=https://github.com/<owner>/<repo>.git`
