# App icon

The master icon is a square image with an ivory background and blue branches.
The committed master has no rounded corners.

| File | Use |
|------|-----|
| `AppIcon-1024.png` | Master artwork |
| `AppIcon.icns` | macOS Dock and Finder |
| `AppIconDev-1024.png`, `AppIconDev.icns` | Development icons with a DEV ribbon, used by `./dev.sh` |

Regenerate the `.icns` from the committed master (from the repo root, needs Pillow):

```bash
python3 -c 'from PIL import Image; Image.open("assets/icon/AppIcon-1024.png").convert("RGBA").save("assets/icon/AppIcon.icns", format="ICNS")'
```
