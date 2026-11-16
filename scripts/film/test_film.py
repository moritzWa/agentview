#!/usr/bin/env python3
"""uv run --with pillow --with resvg-py scripts/film/test_film.py"""

import unittest
from html.parser import HTMLParser

import narrate
import render
import verify
from PIL import ImageStat


class FilmTests(unittest.TestCase):
    def test_edit_is_exactly_thirty_seconds(self):
        self.assertEqual(render.FPS * render.SECONDS, 900)
        self.assertEqual((render.W, render.H), (1600, 900))

    def test_easing_is_bounded_and_monotonic(self):
        values = [render.ease(i / 100) for i in range(-20, 121)]
        self.assertEqual(values, sorted(values))
        self.assertEqual((values[0], values[-1]), (0, 1))

    def test_voice_cues_are_ordered(self):
        cues = [c[0] for c in narrate.LINES]
        self.assertEqual(cues, sorted(set(cues)))
        self.assertLess(cues[-1], 30)
        self.assertEqual(narrate.LINES[3][1], "Tell it checkout.")

    def test_caption_millisecond_rollover(self):
        self.assertEqual(verify.stamp(59.9999), "00:01:00.000")
        self.assertEqual(verify.stamp(3.25), "00:00:03.250")

    def test_full_coupon_choice_fits_two_lines(self):
        self.assertEqual(len(render.VOICE_CHOICE), 2)
        for line in render.VOICE_CHOICE:
            self.assertLess(render.font(30, True).getlength(line), 936)
        self.assertIn("cart or at checkout.", render.VOICE_CHOICE[1])

    def test_brand_assets_render(self):
        for asset in ["openai.png", "claude.svg", "devin.png", "opencode.svg"]:
            icon = render.provider(asset, 44)
            self.assertLessEqual(icon.width, 44)
            self.assertLessEqual(icon.height, 44)
            self.assertGreater(ImageStat.Stat(icon).sum[3], 0)

    def test_safe_player_does_not_autoplay(self):
        html = __import__("pathlib").Path(__file__).with_name("review.html").read_text()

        class Parser(HTMLParser):
            attributes = None

            def handle_starttag(self, tag, attrs):
                if tag == "video":
                    self.attributes = dict(attrs)

        parser = Parser()
        parser.feed(html)
        self.assertIn("controls", parser.attributes)
        self.assertIn("muted", parser.attributes)
        self.assertNotIn("autoplay", parser.attributes)
        self.assertNotIn(".play()", html)


if __name__ == "__main__":
    unittest.main()
