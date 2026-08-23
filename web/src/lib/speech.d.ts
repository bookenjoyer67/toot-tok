// Minimal Web Speech API typings (Safari/Chrome prefix; TS DOM lacks it).
interface SpeechRecognitionEventLike {
	resultIndex: number;
	results: ArrayLike<{
		isFinal: boolean;
		0: { transcript: string };
	}>;
}

interface SpeechRecognitionLike {
	lang: string;
	interimResults: boolean;
	continuous: boolean;
	onresult: ((e: SpeechRecognitionEventLike) => void) | null;
	onend: (() => void) | null;
	onerror: (() => void) | null;
	start(): void;
	stop(): void;
	abort(): void;
}

interface Window {
	SpeechRecognition?: new () => SpeechRecognitionLike;
	webkitSpeechRecognition?: new () => SpeechRecognitionLike;
}
