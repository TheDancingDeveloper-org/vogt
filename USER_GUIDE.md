# MyDevEnv2 User Guide

## Quick Start

### First Launch
1. Open https://mydevenv2.sprooty.com
2. Enter your bearer token in Settings (⚙)
3. Click "Save & reload"

### Create Your First Session
- Click **+ New Session** button
- Choose a template (Shell, Node Dev, Rust Build, Python Env)
- Enter a session name
- Start working!

---

## Features Overview

### 🖥️ Terminal Management
- **Multiple sessions** - Run many terminals simultaneously
- **Persistent scrollback** - Scroll back through command history
- **Session templates** - Pre-configured environments for common tasks
- **Split panes** - Side-by-side terminals (desktop client only)
- **Activity indicators** - Visual feedback for running commands

### ⌨️ Keyboard Shortcuts

#### Global Navigation
- `Ctrl+K` / `Cmd+K` - Open command palette
- `Ctrl+Shift+T` - New terminal session
- `Ctrl+Shift+W` - Close active tab
- `Ctrl+Alt+←` / `→` - Cycle through tabs

#### Terminal
- `Ctrl+Shift+C` / `Cmd+C` - Copy selection
- `Ctrl+Shift+V` / `Cmd+V` - Paste
- `Ctrl+Shift+A` / `Cmd+A` - Select all
- **Middle-click** - Paste (Linux convention)
- **Right-click** - Copy if selection, else paste
- **Pinch gesture** - Zoom font size (mobile/trackpad)

#### Editor
- `Ctrl+S` / `Cmd+S` - Save file
- `Ctrl+F` - Find
- `Ctrl+H` - Find and replace
- All VS Code shortcuts work!

#### Command Palette
- `↑` / `↓` - Navigate
- `Enter` - Execute
- `Esc` - Close

### 📝 Code Editing

#### Monaco Editor Integration
MyDevEnv2 uses the Monaco editor (same engine as VS Code):
- Syntax highlighting for 50+ languages
- IntelliSense / autocomplete
- Find and replace
- Multi-cursor editing
- Auto-save on edit

#### IDE Layout Mode
Switch to IDE mode for code-focused work:
1. Open Settings (⚙)
2. Select **IDE Mode**
3. Click "Save & reload"

**IDE Mode Features:**
- Persistent file tree sidebar (collapsible)
- Editor area with file tabs
- Separate from terminal tabs
- Click sidebar files to open

**Switch back to Tabbed Mode** anytime from Settings.

### 🔍 Command Palette (Ctrl+K)

Fast keyboard-driven navigation:
- **Search sessions** - Type to filter
- **Jump to files** - Open recent files
- **Run commands** - New session, git status, etc.
- **Fuzzy matching** - Partial names work

Examples:
- Type "shell" → finds sessions with "shell" in name
- Type "gst" → matches "git status"

### 📋 Session Templates

Pre-configured environments for quick setup:

| Template | Environment |
|----------|-------------|
| **Shell** | Default bash session |
| **Node Dev** | `NODE_ENV=development` |
| **Rust Build** | `RUST_BACKTRACE=1` |
| **Python Env** | `PYTHONUNBUFFERED=1` |

#### Custom Templates
Add your own in `mydevenv2.toml`:

```toml
[[session_templates]]
name = "Django Dev"
description = "Django development environment"
command = ["bash"]
cwd = "~/projects/myapp"
env = [
    ["DJANGO_SETTINGS_MODULE", "myapp.settings.dev"],
    ["DEBUG", "1"]
]
```

### 📁 File Browser

Access files from the drawer (☰ menu):
- Browse workspace directory tree
- Click files to open in editor
- Click folders to expand/collapse
- **"Open terminal here"** button (>_) on folders
- **Download** button (⬇) on files

### 🔄 Git Integration

View git status without leaving MyDevEnv2:
1. Click **Git** tab
2. See uncommitted changes
3. Click a file to view diff
4. See recent commits below

**Note:** Currently read-only. Use terminal for commits/pushes.

### 📜 Session History (Beta)

Archive and search through past sessions:
- Automatic archiving of completed sessions
- Full-text search across terminal output
- Session replay capability
- 90-day retention (configurable)

Access via command palette: search for "history"

### 📱 Mobile Support

#### Copy/Paste on Mobile
- **Long-press** terminal to select text
- **Right-click/long-press** empty space to paste
- iOS: Use paste modal if clipboard access denied

#### Modifier Keys
On-screen row provides:
- `Esc` `Tab` `Ctrl` (sticky)
- Arrow keys
- Common symbols: `/` `|` `~`
- `Enter`

**Sticky Ctrl:** Tap Ctrl, then tap letter → sends Ctrl+letter

#### Font Sizing
- **Pinch gesture** to zoom terminal font
- Settings → adjust default size

### 🔔 Push Notifications

Get notified when sessions need input:

1. Open Settings (⚙)
2. Click **Enable push**
3. Allow browser permission
4. Test with **Send test** button

**Mobile:** Notifications work even when app is closed.

### 🎨 Customization

#### Settings (⚙ icon)
- Bearer token
- Backend URL (for custom deployments)
- Layout mode (Tabbed vs IDE)
- Push notifications

#### Terminal Preferences
- Font size stored per device
- Pinch to zoom persists

---

## Tips & Tricks

### Productivity Hacks
1. **Use Ctrl+K for everything** - Faster than clicking
2. **Create template for each project** - One-click setup
3. **IDE mode for coding sessions** - Focus on files
4. **Tabbed mode for terminal work** - Quick access to multiple shells
5. **Right-click terminal** - Quickest copy/paste

### Troubleshooting

#### Copy/Paste Not Working
1. Check if HTTPS (required for clipboard API)
2. Grant clipboard permissions in browser
3. Fallback: right-click always works
4. Mobile: use paste modal

#### Session Won't Start
1. Check bearer token in Settings
2. Verify backend URL if using custom deployment
3. Check browser console for errors

#### Editor Won't Save
1. Check file permissions in workspace
2. Look for error message in editor toolbar
3. File might be read-only

#### Command Palette Empty
1. Sessions must be created first
2. Try refreshing the page
3. Check connection to backend

### Best Practices

#### Session Management
- **Name sessions descriptively** - "web-server", "build-watch", "tests"
- **Use templates** - Don't configure env vars manually
- **Close unused sessions** - Keep UI clean
- **Duplicate sessions** - Right-click in session list

#### File Editing
- **Auto-save is on** - No need to save repeatedly
- **Use IDE mode for projects** - Better file navigation
- **Ctrl+F to search** - Works like VS Code

#### Performance
- **Close old tabs** - Don't keep 20+ tabs open
- **Clear old sessions** - Delete finished sessions
- **Use command palette** - Faster than drawer

---

## Keyboard Reference (Full List)

### Navigation
| Shortcut | Action |
|----------|--------|
| `Ctrl/Cmd+K` | Open command palette |
| `Ctrl/Cmd+Shift+T` | New terminal session |
| `Ctrl/Cmd+Shift+W` | Close active tab |
| `Ctrl/Cmd+Alt+←/→` | Cycle tabs |

### Terminal
| Shortcut | Action |
|----------|--------|
| `Ctrl/Cmd+Shift+C` | Copy selection |
| `Ctrl/Cmd+Shift+V` | Paste |
| `Ctrl/Cmd+Shift+A` | Select all |
| `Ctrl+C` (with selection) | Copy (Windows) |
| Middle-click | Paste (Linux) |
| Right-click | Copy or paste |

### Editor
| Shortcut | Action |
|----------|--------|
| `Ctrl/Cmd+S` | Save file |
| `Ctrl/Cmd+F` | Find |
| `Ctrl/Cmd+H` | Find and replace |
| `Ctrl/Cmd+G` | Go to line |
| `Ctrl/Cmd+/` | Toggle comment |
| `Alt+↑/↓` | Move line up/down |
| `Ctrl/Cmd+D` | Add cursor to next match |

---

## FAQ

**Q: Can I use this offline?**  
A: No, MyDevEnv2 requires connection to the server.

**Q: Where are my files stored?**  
A: On the server in your workspace directory (typically `~/Working`).

**Q: Can I access from multiple devices?**  
A: Yes! Sessions persist on the server. Open from any device.

**Q: What happens if I close the browser?**  
A: Sessions keep running on the server. Reconnect anytime.

**Q: Can I use this on tablet/phone?**  
A: Yes! Mobile WebView and native Android app available.

**Q: How do I update?**  
A: PWA auto-updates. Hard refresh (Ctrl+Shift+R) if needed.

**Q: Can I customize the theme?**  
A: Terminal themes coming soon. Editor uses Monaco defaults.

**Q: Is there vim mode?**  
A: Not in the UI, but you can run vim in a terminal session.

**Q: Can multiple people share one instance?**  
A: Not recommended. Each user should have their own instance.

**Q: How do I backup my work?**  
A: Use git! Everything is in your workspace directory.

---

## Advanced Usage

### Custom Backend Deployment
1. Deploy server with your own token
2. Set `MYDEVENV2_TOKEN` environment variable
3. In Settings, set backend URL
4. Enter your token

### Template Inheritance
Templates can reference environment variables:

```toml
[[session_templates]]
name = "Project X"
command = ["bash", "-c", "cd $PROJECT_DIR && npm run dev"]
env = [["PROJECT_DIR", "/workspace/project-x"]]
```

### Workspace Configuration
Server configuration in `mydevenv2.toml`:

```toml
workspace_root = "/home/user/Working"
default_shell = "/bin/bash"
default_cwd = "/home/user/Working"
activity_idle_after_ms = 1500
scrollback_bytes = 4194304  # 4 MiB
```

---

## Getting Help

- **Documentation:** `README.md` in the repository
- **Issues:** Report bugs on Forgejo/GitHub
- **Logs:** Browser console (F12) for client errors
- **Server logs:** Check server output for backend issues

---

*Last updated: 2026-06-25*
