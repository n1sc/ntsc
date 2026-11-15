# Neutron extension identity

**Ultraviolet Glass** is the approved extension and README icon.

- `neutron.svg`: canonical vector icon with its paired purple background.
- `neutron.png`: 512 × 512 extension-store and editor README icon.
- `neutron-file-white.svg`: the same mark in white, on transparency, for `.nt`
  file icons. It has no background, gradient, or shadow.

The background has a 64 px corner radius on its 512 px canvas; the PNG corners
are transparent. It runs from `#473564` to `#212D50`. The translucent mark runs
through `#F2E7FF`, `#BEA8F4`, `#7664BB`, and `#D7DDFF`, with a white reflection.
Both light and dark VS Code language icons use the white variant as requested.

After editing the canonical SVG, render the PNG and regenerate editor copies:

```console
rsvg-convert --width 512 --height 512 extensions/shared/branding/neutron.svg \
  --output extensions/shared/branding/neutron.png
python3 extensions/shared/generate.py
```

The icons are used only by extensions and READMEs. No compiler code, generated
HTML documentation, or native installer branding uses these assets.
Editors without a supported extension icon field show the icon in their README;
the integrations do not change the user's global file-icon theme.
