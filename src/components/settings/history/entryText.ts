import type { HistoryEntry } from "@/bindings";

const hasText = (text: string | null): text is string =>
  text !== null && text.trim() !== "";

/**
 * The text a history entry shows, copies and starts an edit from: the user's
 * own fix if there is one, else what Handy pasted, else the transcript.
 */
export const displayedText = (entry: HistoryEntry): string => {
  if (hasText(entry.edited_text)) return entry.edited_text;
  if (hasText(entry.pasted_text)) return entry.pasted_text;
  return entry.transcription_text;
};
