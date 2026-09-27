# App icon

The master icon is a square image with an ivory background and blue branches.
The artwork has no rounded corners; macOS controls the displayed icon shape.

| File | Use |
|------|-----|
| `AppIcon-1024.png` | Master artwork |
| `AppIcon.icns` | macOS Dock and Finder |
| `AppIconDev-1024.png`, `AppIconDev.icns` | Development icons with a DEV ribbon, used by `./dev.sh` |

Regenerate the `.icns` from the square master (from the repo root, needs Pillow):

```bash
python3 -c 'from PIL import Image; Image.open("assets/icon/AppIcon-1024.png").convert("RGBA").save("assets/icon/AppIcon.icns", format="ICNS")'
```
