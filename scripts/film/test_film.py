#!/usr/bin/env python3
"""uv run --with pillow --with resvg-py --with numpy scripts/film/test_film.py"""

import unittest
from html.parser import HTMLParser

import narrate
import numpy as np
import render
import verify
import visual
import waveform
from PIL import ImageStat


class FilmTests(unittest.TestCase):
    def test_waveform_is_flat_for_silence_and_larger_for_loud_voice(self):
        rate = 48000
        t = np.arange(rate) / rate
        signal = np.sin(2 * np.pi * 440 * t)
        pcm = np.concatenate(
            [np.zeros(rate), signal * 0.01, signal * 0.4, np.zeros(rate)]
        )
        levels = waveform.envelope(pcm)
        self.assertEqual(float(levels[:30].max()), 0)
        self.assertEqual(float(levels[90:].max()), 0)
        self.assertGreater(float(levels[60:90].max()), float(levels[30:60].max()) * 5)
        np.testing.assert_array_equal(levels, waveform.envelope(pcm))

    def test_waveform_obeys_speaker_and_absolute_cue_offset(self):
        tracks = waveform.VoiceTracks(
            [
                {"at": 7.0, "speaker": "YOU", "levels": np.ones((30, 31))},
                {"at": 9.0, "speaker": "AGENT VIEW", "levels": np.full((30, 31), 0.5)},
            ]
        )
        self.assertEqual(tracks.at(6.99, "YOU").max(), 0)
        self.assertEqual(tracks.at(7.5, "YOU").max(), 1)
        self.assertEqual(tracks.at(7.5, "AGENT VIEW").max(), 0)
        self.assertEqual(tracks.at(8.1, "YOU").max(), 0)
        self.assertEqual(tracks.at(9.5, "AGENT VIEW").max(), 0.5)
        self.assertEqual(tracks.at(9.5, "YOU").max(), 0)

    def test_logo_embeds_unchanged_canonical_svg(self):
        self.assertIn(
            visual.uri(visual.ROOT / "docs/assets/logo.svg"), visual.mark(100, 100, 2)
        )
        self.assertNotIn("<circle", visual.mark(100, 100, 2))

    def test_ctrl_s_is_not_labeled_a_configured_global_shortcut(self):
        film = visual.Film.__new__(visual.Film)
        film.terminal = lambda *args, **kwargs: ""
        self.assertNotIn("Global shortcut configured", film.work(19.0))
        self.assertIn("Global shortcut configured", film.work(20.0))

    def test_all_eighteen_supported_harnesses_have_artwork(self):
        self.assertEqual(len(visual.HARNESSES), 18)
        self.assertEqual(len({name for name, _ in visual.HARNESSES}), 18)
        for _, asset in visual.HARNESSES:
            self.assertTrue((visual.ROOT / "site/providers" / asset).is_file())

    def test_edit_is_exactly_thirty_seconds(self):
        self.assertEqual(render.FPS * render.SECONDS, 900)
        self.assertEqual((render.W, render.H), (3840, 2160))

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
