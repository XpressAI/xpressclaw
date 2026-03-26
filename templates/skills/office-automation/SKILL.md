---
name: office-automation
description: Create and edit Office documents (Word, Excel, PowerPoint) using the installed Office apps on the host machine. Use when the user asks to create, edit, or export documents. Works via AppleScript (macOS) or COM/PowerShell (Windows).
---

# Office Document Automation

You can create and edit Word, Excel, and PowerPoint documents using the actual Office applications installed on the user's machine. This gives full fidelity — the real app handles formatting, styles, and rendering.

## Tools Available

- `office_run(app, script, file_path)` — Run a script against an Office app
- `office_read(file_path)` — Extract text content from a document
- `office_export(file_path, format)` — Export a document (e.g., to PDF)

## How It Works

The scripts run on the **host machine** (not in your container). On macOS, you write AppleScript. On Windows, you write PowerShell with COM objects. The xpressclaw server executes the script and returns the result.

## macOS: AppleScript Examples

### Create a Word Document

```applescript
tell application "Microsoft Word"
  activate
  set newDoc to make new document
  set content of text object of newDoc to "Hello, World!"

  -- Add a heading
  set myRange to create range newDoc start 0 end 0
  insert text "My Report" & return at myRange
  set font size of font object of myRange to 24
  set bold of font object of myRange to true

  save as newDoc file name POSIX file "/Users/me/Desktop/report.docx"
end tell
```

### Create an Excel Spreadsheet

```applescript
tell application "Microsoft Excel"
  activate
  set wb to make new workbook
  set ws to active sheet of wb

  -- Headers
  set value of cell "A1" of ws to "Name"
  set value of cell "B1" of ws to "Amount"
  set value of cell "C1" of ws to "Date"
  set bold of font object of cell "A1" of ws to true
  set bold of font object of cell "B1" of ws to true
  set bold of font object of cell "C1" of ws to true

  -- Data
  set value of cell "A2" of ws to "Widget A"
  set value of cell "B2" of ws to 150.00
  set value of cell "A3" of ws to "Widget B"
  set value of cell "B3" of ws to 275.50

  -- Formula
  set value of cell "B4" of ws to "=SUM(B2:B3)"

  save wb in POSIX file "/Users/me/Desktop/sales.xlsx"
end tell
```

### Create a PowerPoint Presentation

```applescript
tell application "Microsoft PowerPoint"
  activate
  set pres to make new presentation

  -- Title slide
  set slide1 to make new slide at end of pres with properties {layout:slide layout title slide}
  set content of text range of text frame of shape 1 of slide1 to "Quarterly Review"
  set content of text range of text frame of shape 2 of slide1 to "Q1 2026 Results"

  -- Content slide
  set slide2 to make new slide at end of pres with properties {layout:slide layout title and content}
  set content of text range of text frame of shape 1 of slide2 to "Key Metrics"
  set content of text range of text frame of shape 2 of slide2 to "• Revenue: $1.2M
• Growth: 15%
• Customers: 450"

  save pres in POSIX file "/Users/me/Desktop/review.pptx"
end tell
```

### Edit an Existing Document

```applescript
tell application "Microsoft Word"
  open POSIX file "/Users/me/Desktop/report.docx"

  -- Find and replace
  tell find object of selection
    set content to "old text"
    set replacement text of replacement object to "new text"
    execute find replace replace all
  end tell

  -- Add text at the end
  set myRange to create range active document start (end of content of active document) end (end of content of active document)
  insert text return & "Added paragraph." at myRange

  save active document
  close active document
end tell
```

## Windows: PowerShell/COM Examples

### Create a Word Document

```powershell
$word = New-Object -ComObject Word.Application
$word.Visible = $true
$doc = $word.Documents.Add()

# Add heading
$selection = $word.Selection
$selection.Style = $doc.Styles.Item("Heading 1")
$selection.TypeText("My Report")
$selection.TypeParagraph()

# Add body text
$selection.Style = $doc.Styles.Item("Normal")
$selection.TypeText("This is the report content.")

$doc.SaveAs2("C:\Users\me\Desktop\report.docx")
$doc.Close()
$word.Quit()
```

### Create an Excel Spreadsheet

```powershell
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $true
$wb = $excel.Workbooks.Add()
$ws = $wb.ActiveSheet

# Headers
$ws.Cells(1,1).Value = "Name"
$ws.Cells(1,2).Value = "Amount"
$ws.Range("A1:B1").Font.Bold = $true

# Data
$ws.Cells(2,1).Value = "Widget A"
$ws.Cells(2,2).Value = 150.00
$ws.Cells(3,1).Value = "Widget B"
$ws.Cells(3,2).Value = 275.50

# Formula
$ws.Cells(4,2).Formula = "=SUM(B2:B3)"

$wb.SaveAs("C:\Users\me\Desktop\sales.xlsx")
$wb.Close()
$excel.Quit()
```

## Rules

- File paths must be absolute paths on the host machine
- The Office app will open visually on the user's screen
- Always close documents after editing to avoid locking
- Use `office_read` first to understand existing document structure before editing
- Use `office_export` to convert to PDF — don't try to create PDFs directly
