#!/usr/bin/env python3
"""Launcher index and category precedence with isolated XDG fixtures."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def launcher(root, relative, text):
    path = root / "applications" / f"{relative}.desktop"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("[Desktop Entry]\n" + text, encoding="utf-8")
    return path


def test(binary, root):
    home, config, data, first, second = (root / name for name in ("home", "config", "data", "first", "second"))
    env = {**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(config),
           "XDG_DATA_HOME": str(data), "XDG_DATA_DIRS": f"{first}:relative:{second}"}

    def app_info(window_class, category, source, *, name=None, app_id=None):
        response = subprocess.run([binary, "app-info", window_class], env=env, cwd=root,
                                  capture_output=True, text=True, check=True, timeout=5)
        fields = dict(line.split(None, 1) for line in response.stdout.splitlines())
        expected = {"Category": category, "Source": source}
        if name is not None:
            expected["Name"] = name
        if app_id is not None:
            expected["Class"] = app_id
        assert all(fields.get(key) == value for key, value in expected.items()), (window_class, fields, expected)

    # The user launcher shadows the system ID and all of its window-class claims.
    launcher(first, "org.test.Nimbus", "Type=Application\nName=System Nimbus\nStartupWMClass=old-nimbus-window\nCategories=Video;\n")
    launcher(data, "org.test.Nimbus", "Type=Application\nName=Nimbus\\sWeb\nStartupWMClass=NIMBUS-WINDOW\nCategories=Network;WebBrowser;\n")
    app_info("ORG.TEST.NIMBUS", "Browsing", "desktop-entry", name="Nimbus Web", app_id="org.test.nimbus")
    app_info("org.test.nimbus.desktop", "Browsing", "desktop-entry")
    app_info("Nimbus-Window", "Browsing", "desktop-entry", name="Nimbus Web", app_id="nimbus-window")
    app_info("old-nimbus-window", "Other", "unclassified")

    # Order in the system data path matters; relative paths do not participate.
    launcher(second, "org.test.Workbench", "Type=Application\nName=Player\nCategories=AudioVideo;\n")
    launcher(first, "org.test.Workbench", "Type=Application\nName=Workbench\nCategories=Development;IDE;\n")
    launcher(root / "relative", "org.test.Relative", "Type=Application\nName=Relative\nCategories=Game;\n")
    app_info("org.test.workbench", "Productivity", "desktop-entry", name="Workbench")
    app_info("org.test.relative", "Other", "unclassified")

    # A Hidden marker masks the other file; NoDisplay doesn't hide a running app.
    launcher(data, "org.test.Masked", "Hidden=true\n")
    launcher(first, "org.test.Masked", "Type=Application\nName=Masked\nStartupWMClass=masked-window\nCategories=WebBrowser;\n")
    app_info("org.test.masked", "Other", "unclassified")
    app_info("masked-window", "Other", "unclassified")
    target = launcher(root / "installed", "scene", "Type=Application\nNoDisplay=true\nName=Scene Studio\nCategories=Graphics;AudioVideoEditing;\n")
    export = home / ".local/share/flatpak/exports/share/applications/artist/scene.desktop"
    export.parent.mkdir(parents=True)
    export.symlink_to(target)
    app_info("artist-scene", "Productivity", "desktop-entry", name="Scene Studio")

    # Conflicting StartupWMClass or case-folded file IDs are unsafe guesses.
    launcher(first, "org.test.Web", "Type=Application\nName=Web\nStartupWMClass=shared-window\nCategories=WebBrowser;\n")
    launcher(second, "org.test.Mail", "Type=Application\nName=Mail\nStartupWMClass=shared-window\nCategories=Network;Email;\n"
             "MimeType=text/html;x-scheme-handler/http;x-scheme-handler/https;\n")
    launcher(first, "org.test.CaseTest", "Type=Application\nName=Web\nCategories=WebBrowser;\n")
    launcher(second, "org.test.casetest", "Type=Application\nName=Mail\nCategories=Email;\n")
    app_info("SHARED-WINDOW", "Other", "unclassified")
    app_info("org.test.web", "Browsing", "desktop-entry")
    app_info("org.test.mail", "Communication", "desktop-entry")
    app_info("org.test.casetest", "Other", "unclassified")

    # Don't treat a network utility with 'Browser' in its name, a helper's
    # executable or a secondary action as a browser.
    launcher(data, "org.test.PhotoBrowser", "Type=Application\nName=Photo Browser\nExec=nebulous-test-executable %U\nCategories=Network;\n"
             "[Desktop Action Unsupported]\nCategories=WebBrowser;\n")
    app_info("org.test.photobrowser", "Other", "unclassified", name="Photo Browser")
    app_info("nebulous-test-executable", "Other", "unclassified")
    launcher(data, "org.test.Link", "Type=Link\nName=Not an app\nCategories=WebBrowser;\n")
    app_info("org.test.link", "Other", "unclassified")
    fifo = root / "fifo"
    os.mkfifo(fifo)
    fifo_link = data / "applications" / "org.test.Pipe.desktop"
    fifo_link.symlink_to(fifo)
    app_info("org.test.pipe", "Other", "unclassified")

    # An accidental launcher category cannot change a known app's category.
    launcher(data, "kitty", "Type=Application\nName=Wrong\nCategories=Game;\n")
    app_info("kitty", "Utilities", "built-in", name="Kitty")
    app_info("net.imput.Helium", "Browsing", "built-in", name="Helium", app_id="net.imput.helium")
    config_file = config / "irminsul" / "config.toml"
    config_file.parent.mkdir(parents=True)
    config_file.write_text("[apps]\n'org.test.nimbus'={category='Utilities'}\n"
                           "'helium-browser'={name='Helium Work',category='Productivity',icon='code'}\n", encoding="utf-8")
    app_info("org.test.nimbus", "Utilities", "config", name="Nimbus Web")
    app_info("helium-browser", "Productivity", "config", name="Helium Work")

    # Relative XDG_DATA_HOME falls back to the freedesktop user-data default.
    launcher(home / ".local/share", "org.test.DefaultHome", "Type=Application\nName=Note Desk\nCategories=Office;\n")
    env["XDG_DATA_HOME"] = "relative/data"
    app_info("org.test.defaulthome", "Productivity", "desktop-entry", name="Note Desk")
    assert not (data / "irminsul").exists() and not (home / ".local/share/irminsul").exists()


if __name__ == "__main__":
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/irminsul").resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="irminsul-categories-") as root:
        test(binary, Path(root))
    print("OK: exact IDs, startup classes, XDG and Flatpak launchers, ambiguity, fallbacks and override precedence")
