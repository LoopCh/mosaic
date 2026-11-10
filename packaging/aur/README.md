# AUR package `mosaic-vpn-bin`

Arch Linux users install Mosaic with any AUR helper:

```sh
paru -S mosaic-vpn-bin      # or: yay -S mosaic-vpn-bin
```

The package repackages the official `.deb` from GitHub Releases. The name `mosaic`
is taken in the AUR (the NCSA Mosaic browser), hence `mosaic-vpn`.

## Publishing (once)

1. Create an account at https://aur.archlinux.org.
2. Make a key without a passphrase just for the AUR:
   `ssh-keygen -t ed25519 -N "" -C aur -f ~/.ssh/aur`
   and paste `~/.ssh/aur.pub` into *My Account → SSH Public Key*.
3. In the GitHub repository → Settings → Secrets and variables → Actions add:
   - `AUR_SSH_PRIVATE_KEY` — the contents of `~/.ssh/aur`
   - `AUR_USERNAME` — your AUR user name
   - `AUR_EMAIL` — e-mail for the AUR commits
4. Push a release tag (`v0.9.0`, …). The `aur-publish` job in `release.yml` fills in the
   version and SHA-256 of the `.deb`, checks that it builds (`makepkg`) and pushes to the
   AUR. The first push creates the package. Without the secrets the job does nothing.

## Testing locally on Arch

```sh
cd packaging/aur/mosaic-vpn-bin
updpkgsums          # pacman-contrib; replaces SKIP with the real checksum
makepkg -si
```
