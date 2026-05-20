#!/usr/bin/env python3
"""
test_endpoint.py — quick health check for the LLM endpoint.

Fires:
  1. A trivial text request (cold — endpoint warm-up)
  2. The same request again (warm — should be faster)
  3. A vision request with a 1×1 PNG (confirms multimodal works)

Reads OPENAI_API_KEY, OPENAI_BASE_URL, MODEL from env (same as
android_pilot.py).

Usage:
    .venv/bin/python test_endpoint.py
"""

import base64
import os
import sys
import time

from dotenv import load_dotenv
from openai import OpenAI

load_dotenv()  # auto-load ./.env

# Smallest possible PNG: 1×1 transparent pixel.
TINY_PNG_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjC"
    "B0C8AAAAASUVORK5CYII="
)


def timed(label, fn):
    print(f"[{label}] starting...")
    t0 = time.perf_counter()
    try:
        resp = fn()
        dt = time.perf_counter() - t0
        msg = resp.choices[0].message
        content = (msg.content or "").strip()
        # Some models (Qwen3.5, R1, o1) emit a separate `reasoning_content`
        # field with their thinking before the answer.
        reasoning = getattr(msg, "reasoning_content", None) or ""
        usage = resp.usage
        suffix = f" [reasoning: {len(reasoning)} chars, "
        suffix += f"{usage.completion_tokens} completion tokens]"
        print(f"[{label}] OK in {dt:.2f}s → {content!r}{suffix}")
        return dt
    except Exception as e:
        dt = time.perf_counter() - t0
        print(f"[{label}] FAIL in {dt:.2f}s: {type(e).__name__}: {e}")
        return None


def main():
    api_key = os.getenv("OPENAI_API_KEY")
    base_url = os.getenv("OPENAI_BASE_URL",
                         "https://dashscope-intl.aliyuncs.com/compatible-mode/v1")
    model = os.getenv("MODEL", "qwen3-vl-plus")
    if not api_key:
        sys.exit("error: $OPENAI_API_KEY not set")

    print(f"base_url: {base_url}")
    print(f"model:    {model}")
    print()

    client = OpenAI(base_url=base_url, api_key=api_key)

    # max_tokens generous enough to clear the reasoning trace and still
    # leave room for an answer (Qwen3.5 27b spent ~180 tokens thinking).
    MAX = 1024

    def text_call():
        return client.chat.completions.create(
            model=model,
            messages=[{"role": "user", "content": "Say 'pong'. Reply in exactly one word."}],
            max_tokens=MAX,
        )

    def vision_call():
        return client.chat.completions.create(
            model=model,
            messages=[{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image? One word."},
                    {"type": "image_url",
                     "image_url": {"url": f"data:image/png;base64,{TINY_PNG_B64}"}},
                ],
            }],
            max_tokens=MAX,
        )

    t_cold = timed("text-cold ", text_call)
    t_warm = timed("text-warm ", text_call)
    t_vis = timed("vision    ", vision_call)

    print()
    if t_cold and t_warm:
        delta = t_cold - t_warm
        print(f"warm-up cost: {delta:+.2f}s (cold - warm)")
    if all(x is not None for x in (t_cold, t_warm, t_vis)):
        print("\nall good — endpoint is healthy and multimodal works.")
    else:
        print("\nat least one call failed; see errors above.")
        sys.exit(1)


if __name__ == "__main__":
    main()
