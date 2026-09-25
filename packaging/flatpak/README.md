# Flathub Submission Guide for Unpackr

This directory contains the files required for publishing **Unpackr** to [Flathub](https://flathub.org/), making it directly installable in **GNOME Software**, **KDE Discover**, and via `flatpak install`.

## Files Included

- **`io.github.rahulkumar007.Unpackr.yml`**: Flatpak builder manifest configuring permissions (`--socket=wayland`, `--socket=fallback-x11`, `--device=dri`, `--filesystem=host`).
- **`io.github.rahulkumar007.Unpackr.metainfo.xml`**: AppStream metadata specification (descriptions, license, developer details, categories).
- **`io.github.rahulkumar007.Unpackr.desktop`**: FreeDesktop desktop entry.
- **`io.github.rahulkumar007.Unpackr.svg`**: Application vector icon.

---

## How to Submit to Flathub

1. **Fork the Flathub Repository**:
   Fork [https://github.com/flathub/flathub](https://github.com/flathub/flathub) on GitHub.

2. **Clone Your Fork**:
   ```bash
   git clone git@github.com:RahulKumar-007/flathub.git
   cd flathub
   git checkout -b new-pr/io.github.rahulkumar007.Unpackr
   ```

3. **Add the Package Directory**:
   Create a new directory named after the App ID and copy these files:
   ```bash
   mkdir -p io.github.rahulkumar007.Unpackr
   cp /path/to/unpackr/packaging/flatpak/* io.github.rahulkumar007.Unpackr/
   rm io.github.rahulkumar007.Unpackr/README.md io.github.rahulkumar007.Unpackr/build_flatpak.sh
   ```

4. **Commit & Push**:
   ```bash
   git add io.github.rahulkumar007.Unpackr
   git commit -m "Add io.github.rahulkumar007.Unpackr"
   git push origin new-pr/io.github.rahulkumar007.Unpackr
   ```

5. **Open Pull Request**:
   Open a pull request to `flathub/flathub`. The Flathub build bot (`@flathubbot`) will automatically build and test the Flatpak. Once reviewed and approved by Flathub maintainers, the repository `https://github.com/flathub/io.github.rahulkumar007.Unpackr` is automatically created, and your app will be published on Flathub!
