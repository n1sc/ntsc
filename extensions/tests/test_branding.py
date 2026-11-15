import json
import struct
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BRANDING = ROOT / "extensions/shared/branding"
SVG = {"svg": "http://www.w3.org/2000/svg"}


class BrandingTests(unittest.TestCase):
    def test_app_and_file_icons_share_the_exact_path(self):
        brand = ET.parse(BRANDING / "neutron.svg")
        paths = {path.attrib["d"] for path in brand.findall(".//svg:path", SVG)}
        self.assertEqual(len(paths), 1)
        file_icon = ET.parse(BRANDING / "neutron-file-white.svg")
        self.assertEqual(
            {path.attrib["d"] for path in file_icon.findall(".//svg:path", SVG)}, paths,
        )
        self.assertEqual(file_icon.find("svg:path", SVG).attrib["fill"], "#FFFFFF")
        self.assertFalse(file_icon.findall(".//svg:rect", SVG))
        self.assertFalse(file_icon.findall(".//svg:filter", SVG))
        background = brand.find(".//svg:linearGradient[@id='background']", SVG)
        self.assertEqual(
            [stop.attrib["stop-color"] for stop in background],
            ["#473564", "#212D50"],
        )

    def test_extension_icon_and_white_file_icons_exist(self):
        package = json.loads((ROOT / "extensions/vscode/package.json").read_text())
        extension = ROOT / "extensions/vscode"
        self.assertEqual((extension / package["icon"]).read_bytes(), (BRANDING / "neutron.png").read_bytes())
        language = package["contributes"]["languages"][0]
        for theme in ("dark", "light"):
            self.assertEqual(
                (extension / language["icon"][theme]).read_bytes(),
                (BRANDING / "neutron-file-white.svg").read_bytes(),
            )
        self.assertEqual(package["galleryBanner"]["color"], "#212D50")

    def test_editor_mirrors_ship_the_same_brand_icon(self):
        icon = (BRANDING / "neutron.png").read_bytes()
        for directory in ("vscode", "zed", "nvim", "emacs", "other/vim", "other/sublime", "other/helix"):
            editor = ROOT / "extensions" / directory
            self.assertEqual((editor / "icons/icon.png").read_bytes(), icon, directory)
            self.assertIn('src="icons/icon.png"', (editor / "README.md").read_text())

    def test_png_is_a_square_512_pixel_asset(self):
        image = (BRANDING / "neutron.png").read_bytes()
        self.assertEqual(image[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(struct.unpack(">II", image[16:24]), (512, 512))
        self.assertEqual(image[25], 6)

    def test_background_has_rounded_corners(self):
        brand = ET.parse(BRANDING / "neutron.svg")
        background = brand.find("svg:rect", SVG)
        self.assertEqual(background.attrib["rx"], "64")
        self.assertEqual(background.attrib["ry"], "64")
        self.assertEqual(background.attrib["width"], "512")
        self.assertEqual(background.attrib["height"], "512")

    def test_readme_images_resolve_to_existing_files(self):
        for relative in (
            "README.md", "docs/README.md", "extensions/README.md", "extensions/other/README.md",
        ):
            readme = ROOT / relative
            source = readme.read_text().split('<img src="', 1)[1].split('"', 1)[0]
            self.assertEqual(
                (readme.parent / source).resolve(),
                (BRANDING / "neutron.svg").resolve(),
            )
            self.assertTrue((readme.parent / source).is_file())


if __name__ == "__main__":
    unittest.main()
