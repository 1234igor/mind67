# App icon

An electric-blue branching groove cut into a full-bleed ivory plate. The square
master deliberately has no baked-in rounded corners; macOS decides how an app
icon is presented.

| File | Use |
|------|-----|
| `AppIcon-1024.png` | Master artwork |
| `AppIcon.icns` | macOS Dock and Finder |
| `AppIconDev-1024.png`, `AppIconDev.icns` | The same, wearing a DEV ribbon, for `./dev.sh` |

Regenerate the `.icns` from the square master (from the repo root, needs Pillow):

```bash
python3 -c 'from PIL import Image; Image.open("assets/icon/AppIcon-1024.png").convert("RGBA").save("assets/icon/AppIcon.icns", format="ICNS")'
```
