#!/usr/bin/env python3
"""Generate the bundled .tmTheme color schemes from compact palettes.

Run from the repo root: python3 packaging/gen_themes.py
Writes crates/syntax/themes/*.tmTheme (embedded into the binary).
"""
from pathlib import Path
from xml.sax.saxutils import escape

# Each palette: editor colors + syntax roles.
THEMES = {
    "Monokai": dict(bg="#272822", fg="#F8F8F2", sel="#49483E", line="#3E3D32", caret="#F8F8F0", gutter="#90908A",
        comment="#75715E", string="#E6DB74", number="#AE81FF", constant="#AE81FF", keyword="#F92672", storage="#66D9EF",
        function="#A6E22E", type="#66D9EF", param="#FD971F", variable="#F8F8F2", tag="#F92672", attr="#A6E22E", punct="#F8F8F2", invalid="#F44747"),
    "One Dark": dict(bg="#282C34", fg="#ABB2BF", sel="#3E4451", line="#2C313C", caret="#528BFF", gutter="#4B5263",
        comment="#5C6370", string="#98C379", number="#D19A66", constant="#D19A66", keyword="#C678DD", storage="#C678DD",
        function="#61AFEF", type="#E5C07B", param="#E06C75", variable="#E06C75", tag="#E06C75", attr="#D19A66", punct="#ABB2BF", invalid="#F44747"),
    "Dracula": dict(bg="#282A36", fg="#F8F8F2", sel="#44475A", line="#303241", caret="#F8F8F0", gutter="#6272A4",
        comment="#6272A4", string="#F1FA8C", number="#BD93F9", constant="#BD93F9", keyword="#FF79C6", storage="#FF79C6",
        function="#50FA7B", type="#8BE9FD", param="#FFB86C", variable="#F8F8F2", tag="#FF79C6", attr="#50FA7B", punct="#F8F8F2", invalid="#FF5555"),
    "Catppuccin Mocha": dict(bg="#1E1E2E", fg="#CDD6F4", sel="#45475A", line="#2A2B3C", caret="#F5E0DC", gutter="#6C7086",
        comment="#7F849C", string="#A6E3A1", number="#FAB387", constant="#FAB387", keyword="#CBA6F7", storage="#CBA6F7",
        function="#89B4FA", type="#F9E2AF", param="#EBA0AC", variable="#CDD6F4", tag="#89B4FA", attr="#F9E2AF", punct="#9399B2", invalid="#F38BA8"),
    "Catppuccin Latte": dict(bg="#EFF1F5", fg="#4C4F69", sel="#CCD0DA", line="#E6E9EF", caret="#DC8A78", gutter="#9CA0B0",
        comment="#8C8FA1", string="#40A02B", number="#FE640B", constant="#FE640B", keyword="#8839EF", storage="#8839EF",
        function="#1E66F5", type="#DF8E1D", param="#E64553", variable="#4C4F69", tag="#1E66F5", attr="#DF8E1D", punct="#7C7F93", invalid="#D20F39"),
    "Nord": dict(bg="#2E3440", fg="#D8DEE9", sel="#434C5E", line="#3B4252", caret="#D8DEE9", gutter="#4C566A",
        comment="#616E88", string="#A3BE8C", number="#B48EAD", constant="#B48EAD", keyword="#81A1C1", storage="#81A1C1",
        function="#88C0D0", type="#8FBCBB", param="#D8DEE9", variable="#D8DEE9", tag="#81A1C1", attr="#8FBCBB", punct="#ECEFF4", invalid="#BF616A"),
    "Gruvbox Dark": dict(bg="#282828", fg="#EBDBB2", sel="#504945", line="#32302F", caret="#EBDBB2", gutter="#7C6F64",
        comment="#928374", string="#B8BB26", number="#D3869B", constant="#D3869B", keyword="#FB4934", storage="#FE8019",
        function="#B8BB26", type="#FABD2F", param="#83A598", variable="#83A598", tag="#FB4934", attr="#FABD2F", punct="#EBDBB2", invalid="#FB4934"),
    "Tokyo Night": dict(bg="#1A1B26", fg="#C0CAF5", sel="#283457", line="#1F2335", caret="#C0CAF5", gutter="#3B4261",
        comment="#565F89", string="#9ECE6A", number="#FF9E64", constant="#FF9E64", keyword="#BB9AF7", storage="#9D7CD8",
        function="#7AA2F7", type="#2AC3DE", param="#E0AF68", variable="#C0CAF5", tag="#F7768E", attr="#BB9AF7", punct="#89DDFF", invalid="#F7768E"),
    "GitHub Light": dict(bg="#FFFFFF", fg="#24292F", sel="#C8E1FF", line="#F6F8FA", caret="#0969DA", gutter="#8C959F",
        comment="#6E7781", string="#0A3069", number="#0550AE", constant="#0550AE", keyword="#CF222E", storage="#CF222E",
        function="#8250DF", type="#953800", param="#24292F", variable="#953800", tag="#116329", attr="#0550AE", punct="#24292F", invalid="#82071E"),
    "Mariana": dict(bg="#303841", fg="#D8DEE9", sel="#4E5A65", line="#3A444E", caret="#F9AE58", gutter="#A6ACB9",
        comment="#A6ACB9", string="#99C794", number="#F9AE58", constant="#EC5F66", keyword="#C695C6", storage="#C695C6",
        function="#5FB4B4", type="#F9AE58", param="#F9AE58", variable="#D8DEE9", tag="#EC5F66", attr="#C695C6", punct="#5FB4B4", invalid="#EC5F66"),
}

RULES = [
    ("Comment", "comment, punctuation.definition.comment", "comment", "italic"),
    ("String", "string, punctuation.definition.string", "string", ""),
    ("Number", "constant.numeric", "number", ""),
    ("Constant", "constant.language, constant.character, constant.other, support.constant, variable.other.constant", "constant", ""),
    ("Keyword", "keyword, keyword.control, keyword.operator.word", "keyword", ""),
    ("Operator", "keyword.operator", "punct", ""),
    ("Storage", "storage, storage.type, storage.modifier", "storage", ""),
    ("Function", "entity.name.function, support.function, meta.function-call variable.function, variable.function", "function", ""),
    ("Type", "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, support.type, support.class, entity.other.inherited-class", "type", ""),
    ("Parameter", "variable.parameter", "param", "italic"),
    ("Variable", "variable.other.member, variable.language, variable.other.readwrite.instance", "variable", ""),
    ("Tag", "entity.name.tag, meta.tag", "tag", ""),
    ("Attribute", "entity.other.attribute-name, meta.attribute", "attr", ""),
    ("Punctuation", "punctuation.separator, punctuation.terminator, punctuation.accessor", "punct", ""),
    ("Macro", "support.macro, entity.name.function.macro, meta.macro", "function", ""),
    ("Heading", "markup.heading, entity.name.section", "function", "bold"),
    ("Bold", "markup.bold", "constant", "bold"),
    ("Italic", "markup.italic", "param", "italic"),
    ("Link", "markup.underline.link, string.other.link", "type", ""),
    ("Inserted", "markup.inserted", "string", ""),
    ("Deleted", "markup.deleted", "invalid", ""),
    ("Invalid", "invalid", "invalid", ""),
]


def kv(k, v):
    return f"<key>{escape(k)}</key><string>{escape(v)}</string>"


def theme(name, p):
    settings = [
        "<dict><key>settings</key><dict>"
        + kv("background", p["bg"]) + kv("foreground", p["fg"]) + kv("caret", p["caret"])
        + kv("selection", p["sel"]) + kv("lineHighlight", p["line"]) + kv("gutter", p["bg"])
        + kv("gutterForeground", p["gutter"]) + "</dict></dict>"
    ]
    for title, scope, role, style in RULES:
        inner = kv("foreground", p[role]) + (kv("fontStyle", style) if style else "")
        settings.append(f"<dict>{kv('name', title)}{kv('scope', scope)}<key>settings</key><dict>{inner}</dict></dict>")
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        f'<plist version="1.0"><dict>{kv("name", name)}<key>settings</key><array>\n'
        + "\n".join(settings)
        + "\n</array></dict></plist>\n"
    )


out = Path("crates/syntax/themes")
out.mkdir(parents=True, exist_ok=True)
for name, p in THEMES.items():
    (out / f"{name}.tmTheme").write_text(theme(name, p))
print(f"wrote {len(THEMES)} themes to {out}")
