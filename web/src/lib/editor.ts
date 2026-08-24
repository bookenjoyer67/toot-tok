// Edit pipeline types — client builds an EditManifest, server renders it
// with ffmpeg (Phase 2/3 backend job). Times in SECONDS, all relative to
// each segment's ORIGINAL timeline.

export interface EditSegment {
	/** Blob URL during editing; replaced by uploaded file id at submit. */
	src: string;
	/** Original source duration (from the recorded segment/blob). */
	sourceDuration: number;
	/** Trim window, in source seconds. */
	trimStart: number;
	trimEnd: number;
	/** Playback speed (0.5, 1, 2, 3). */
	speed: number;
	/** 0..1 */
	volume: number;
}

export interface TextOverlay {
	text: string;
	/** 0..1 normalized position within the FINAL frame. */
	x: number;
	y: number;
	/** Font scale relative to default. */
	scale: number;
	/** Start/end in FINAL timeline seconds (across concatenated segments). */
	start: number;
	end: number;
	color: string;
	/** 'center' | 'left' | 'right' */
	align: 'center' | 'left' | 'right';
}

export type FilterId = 'none' | 'vivid' | 'fade' | 'noir' | 'warm' | 'cool' | 'drama';

export interface EditManifest {
	segments: EditSegment[];
	overlays: TextOverlay[];
	filter: FilterId;
	voiceover: { src: string; volume: number } | null;
	music: { src: string; volume: number } | null;
	/** Frame (in final seconds) to use as cover thumbnail. */
	coverTime: number;
	caption: string;
	cwText: string | null;
	/** Optional user-named sound; server defaults to "original sound — @user" when music/VO present. */
	soundTitle?: string | null;
}

/** Length of a segment after trim + speed, in final seconds. */
export function segmentFinalDuration(s: EditSegment): number {
	return Math.max(0, (s.trimEnd - s.trimStart) / s.speed);
}

/** Total final duration of a manifest's segments. */
export function manifestDuration(m: EditManifest): number {
	return m.segments.reduce((acc, s) => acc + segmentFinalDuration(s), 0);
}

/** Map a FINAL timeline time to (segmentIndex, sourceTime, localTime). */
export function resolveTime(m: EditManifest, finalTime: number): {
	segmentIndex: number;
	sourceTime: number;
	localTime: number;
} {
	let acc = 0;
	for (let i = 0; i < m.segments.length; i++) {
		const d = segmentFinalDuration(m.segments[i]);
		if (finalTime < acc + d) {
			const local = finalTime - acc;
			return {
				segmentIndex: i,
				sourceTime: m.segments[i].trimStart + local * m.segments[i].speed,
				localTime: local
			};
		}
		acc += d;
	}
	// Clamp to the very end.
	const last = m.segments.length - 1;
	return {
		segmentIndex: Math.max(0, last),
		sourceTime: m.segments[Math.max(0, last)]?.trimEnd ?? 0,
		localTime: segmentFinalDuration(m.segments[Math.max(0, last)] ?? { trimStart: 0, trimEnd: 0, speed: 1 } as EditSegment) ?? 0
	};
}

/** CSS filter string for a preset — used for real-time client preview. */
export function cssFilter(f: FilterId): string {
	switch (f) {
		case 'vivid':
			return 'saturate(1.45) contrast(1.08)';
		case 'fade':
			return 'contrast(0.9) brightness(1.08) saturate(0.85)';
		case 'noir':
			return 'grayscale(1) contrast(1.15)';
		case 'warm':
			return 'sepia(0.28) saturate(1.2) brightness(1.04)';
		case 'cool':
			return 'hue-rotate(15deg) saturate(1.1) brightness(0.97)';
		case 'drama':
			return 'contrast(1.35) saturate(0.9) brightness(0.94)';
		default:
			return 'none';
	}
}

export const FILTERS: { id: FilterId; label: string }[] = [
	{ id: 'none', label: 'None' },
	{ id: 'vivid', label: 'Vivid' },
	{ id: 'fade', label: 'Fade' },
	{ id: 'noir', label: 'Noir' },
	{ id: 'warm', label: 'Warm' },
	{ id: 'cool', label: 'Cool' },
	{ id: 'drama', label: 'Drama' }
];

export const SPEEDS = [0.5, 1, 2, 3];

/** Max recorded clip length (short-form law). */
export const MAX_CLIP_SECONDS = 60;
