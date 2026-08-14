// File type icon mapping
export function getFileIcon(path: string): string {
  const ext = path.split('.').pop()?.toLowerCase() || '';
  const basename = path.split('/').pop()?.toLowerCase() || '';

  // Special files by name
  const specialFiles: Record<string, string> = {
    'package.json': '📦',
    'package-lock.json': '🔒',
    'cargo.toml': '📦',
    'cargo.lock': '🔒',
    'dockerfile': '🐳',
    'docker-compose.yml': '🐳',
    'readme.md': '📖',
    'license': '📄',
    'makefile': '⚙️',
    '.gitignore': '🚫',
    '.env': '🔐',
  };

  if (specialFiles[basename]) {
    return specialFiles[basename];
  }

  // By extension
  const iconMap: Record<string, string> = {
    // Programming languages
    'js': '📜',
    'jsx': '⚛️',
    'ts': '📘',
    'tsx': '⚛️',
    'rs': '🦀',
    'py': '🐍',
    'java': '☕',
    'go': '🐹',
    'rb': '💎',
    'php': '🐘',
    'c': '©️',
    'cpp': '©️',
    'h': '©️',
    'cs': '#️⃣',
    'swift': '🍎',
    'kt': '🔷',

    // Web
    'html': '🌐',
    'css': '🎨',
    'scss': '🎨',
    'sass': '🎨',
    'less': '🎨',
    'vue': '💚',
    'svelte': '🧡',

    // Data/Config
    'json': '{}',
    'yaml': '📋',
    'yml': '📋',
    'toml': '⚙️',
    'xml': '📄',
    'csv': '📊',

    // Markdown/Docs
    'md': '📝',
    'txt': '📄',
    'pdf': '📕',
    'doc': '📘',
    'docx': '📘',

    // Images
    'png': '🖼️',
    'jpg': '🖼️',
    'jpeg': '🖼️',
    'gif': '🖼️',
    'svg': '🎨',
    'ico': '🖼️',

    // Archives
    'zip': '🗜️',
    'tar': '🗜️',
    'gz': '🗜️',
    'rar': '🗜️',

    // Shell
    'sh': '🐚',
    'bash': '🐚',
    'zsh': '🐚',
    'fish': '🐠',

    // Other
    'sql': '🗄️',
    'db': '🗄️',
    'sqlite': '🗄️',
    'log': '📜',
  };

  return iconMap[ext] || '📄';
}

export function getFolderIcon(isOpen: boolean): string {
  return isOpen ? '📂' : '📁';
}

export function getGitStatusIcon(status?: 'modified' | 'untracked' | 'deleted'): string | null {
  const statusMap = {
    modified: '●',
    untracked: '?',
    deleted: '✕',
  };
  return status ? statusMap[status] : null;
}
