export function timeAgo(iso: string): string {
	const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
	if (seconds < 60) return 'now';
	if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
	if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h`;
	if (seconds < 604_800) return `${Math.floor(seconds / 86_400)}d`;
	return `${Math.floor(seconds / 604_800)}w`;
}
