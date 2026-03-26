---
name: browser-automation
description: Automate web browsers using Playwright. Take screenshots, extract content from JavaScript-heavy pages, fill forms, click buttons, and run custom browser scripts. Use when the user needs to interact with web pages, test web apps, or scrape dynamic content.
---

# Browser Automation

You can control a real web browser (Chrome on the host machine) using Playwright. xpressclaw launches Chrome with remote debugging, and Playwright inside your container connects to it via CDP.

## Tools Available

- `browser_screenshot(url, file_name, wait_for, full_page)` — Take a screenshot
- `browser_fetch(url, selector, wait_for)` — Extract text from JS-rendered pages
- `browser_run(script)` — Run a custom Playwright Python script

## Quick Tools

### Screenshot
```
browser_screenshot(url="https://example.com", file_name="example.png", full_page=True)
```
Screenshots are saved to `/workspace/screenshots/`.

### Fetch Page Content
```
browser_fetch(url="https://news.ycombinator.com", selector="tr.athing .titleline > a")
```
Extracts text from elements matching the CSS selector. Renders JavaScript first.

## Custom Scripts

For complex automation, use `browser_run` with a Playwright script. The browser is already running — connect via CDP:

```python
from playwright.sync_api import sync_playwright
import os

with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp(os.environ["CDP_URL"])
    context = browser.new_context()
    page = context.new_page()

    # Navigate
    page.goto("https://example.com")

    # Extract text
    title = page.text_content("h1")
    print(f"Title: {title}")

    # Fill a form
    page.fill("input#email", "user@example.com")
    page.click("button#submit")

    # Screenshot
    page.screenshot(path=os.environ["SCREENSHOTS_DIR"] + "/result.png")

    context.close()
```

Use `$CDP_URL` and `$SCREENSHOTS_DIR` environment variables in your scripts.

## Rules

- Chrome runs on the host machine — Playwright in the container connects via CDP
- Use `$CDP_URL` to connect: `p.chromium.connect_over_cdp(os.environ["CDP_URL"])`
- Always create a new context and close it when done
- Screenshots saved to `/workspace/screenshots/`
- The browser is visible on the user's screen — they can see what's happening
