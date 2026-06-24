/// 路径太长时"保头保尾"的中段省略——跟 `text-overflow: ellipsis` 的尾部截断
/// 不同，文件路径最有信息量的部分往往同时在两头（盘符/顶层目录 + 文件名），
/// 中间的中间层级目录砍掉最不心疼。纯字符数预算，不做像素测量：11px 等宽
/// 字体下字符数跟视觉宽度基本线性，够用，没必要为了精确到像素引入布局测量。
export function middleEllipsis(text: string, maxChars = 64): string {
	if (text.length <= maxChars) return text;
	const keep = maxChars - 1; // 留一个字符给省略号
	const head = Math.ceil(keep * 0.6);
	const tail = keep - head;
	return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}
