---
name: browser-automation
description: Automate web browsers using Playwright. Take screenshots, extract content from JavaScript-heavy pages, fill forms, click buttons, and run custom browser scripts. Use when the user needs to interact with web pages, test web apps, or scrape dynamic content.
---

# Browser Automation

You can control a real web browser (Chromium) on the host machine using Playwright. This is useful for:
- Taking screenshots of web pages
- Extracting content from JavaScript-rendered pages
- Testing web applications
- Filling forms and clicking buttons
- Scraping data that requires a real browser

## Tools Available

- `browser_screenshot(url, file_name, wait_for, full_page)` — Take a screenshot of a web page
- `browser_fetch(url, selector, wait_for)` — Navigate to a URL and extract text content
- `browser_run(script)` — Run a custom Playwright Python script

## Quick Tools

### Screenshot
```
browser_screenshot(url="https://example.com", file_name="example.png", full_page=True)
```
Screenshots are saved to the agent's screenshots directory.

### Fetch Page Content
```
browser_fetch(url="https://news.ycombinator.com", selector="tr.athing .titleline > a")
```
Extracts text content from elements matching the CSS selector. Unlike HTTP fetch, this renders JavaScript first.

## Custom Scripts

For complex automation, use `browser_run` with a Python script using `playwright.sync_api`:

```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()

    # Navigate
    page.goto("https://example.com")

    # Wait for content
    page.wait_for_selector("h1")

    # Extract text
    title = page.text_content("h1")
    print(f"Title: {title}")

    # Click a button
    page.click("button#submit")

    # Fill a form
    page.fill("input#email", "user@example.com")

    # Screenshot
    page.screenshot(path="$SCREENSHOTS_DIR/result.png")

    browser.close()
```

Use `$SCREENSHOTS_DIR` in your scripts for the output path.

## Common Patterns

### Extract a table
```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    page.goto("https://example.com/data")

    rows = page.query_selector_all("table tr")
    for row in rows:
        cells = row.query_selector_all("td, th")
        print("\t".join(c.text_content() for c in cells))

    browser.close()
```

### Wait for dynamic content
```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    page.goto("https://example.com/app")

    # Wait for AJAX content to load
    page.wait_for_selector(".results-loaded", timeout=15000)

    content = page.text_content(".results")
    print(content)

    browser.close()
```

## Rules

- Scripts run on the **host machine**, not in the container
- Playwright must be installed on the host (`pip install playwright && playwright install chromium`)
- Screenshots are saved to the agent's screenshots directory
- Use `$SCREENSHOTS_DIR` for output paths in custom scripts
- The browser runs headless by default — no window appears
