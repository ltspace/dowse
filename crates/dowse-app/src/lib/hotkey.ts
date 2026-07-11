// 把 tauri-plugin-global-shortcut 认的原始快捷键字符串（如 "Alt+Backquote"）
// 转成人类习惯的显示形式（"Alt+`"）——只覆盖常见的符号键/控制键，覆盖不到的
// 键位（多数是 KeyA~KeyZ、Digit0~9、F1~F12，本身已经可读）走通用规则或原样
// 透传。Rust 侧的 get_hotkey 命令只管传原始值，展示格式的判断收在这一处，
// 不往 commands.rs 里塞展示逻辑。

const CODE_LABELS: Record<string, string> = {
	Backquote: '`',
	Minus: '-',
	Equal: '=',
	BracketLeft: '[',
	BracketRight: ']',
	Backslash: '\\',
	Semicolon: ';',
	Quote: "'",
	Comma: ',',
	Period: '.',
	Slash: '/',
	Space: 'Space',
	Enter: '↵',
	Tab: 'Tab',
	Escape: 'Esc',
	Backspace: 'Backspace',
	Delete: 'Del',
	ArrowUp: '↑',
	ArrowDown: '↓',
	ArrowLeft: '←',
	ArrowRight: '→'
};

function labelOf(segment: string): string {
	if (CODE_LABELS[segment]) return CODE_LABELS[segment];
	// keyboard_types::Code 的序列化命名规律：KeyA -> A，Digit1 -> 1。
	if (/^Key[A-Z]$/.test(segment)) return segment.slice(3);
	if (/^Digit[0-9]$/.test(segment)) return segment.slice(5);
	return segment;
}

/** "Alt+Backquote" -> "Alt+`"；解析不出来的片段原样返回，不让异常输入吞掉展示。 */
export function formatHotkey(raw: string): string {
	if (!raw) return raw;
	return raw
		.split('+')
		.map((part) => part.trim())
		.filter(Boolean)
		.map(labelOf)
		.join('+');
}
