import type { SessionEvent, TaskMessage } from '$lib/api';
import { serverTimestampMs } from '$lib/serverTime';

const MAX_FRAGMENT_GAP_MS = 2_000;

function isAgentMessage(event: SessionEvent): boolean {
	return event.event_type === 'runner_progress'
		&& event.payload?.item_type === 'agent_message';
}

function isSameAgentMessageStream(previous: SessionEvent, next: SessionEvent): boolean {
	if (!previous.attempt_id || previous.attempt_id !== next.attempt_id) return false;
	if (previous.session_id !== next.session_id) return false;
	if (previous.source_type !== next.source_type || previous.source_id !== next.source_id) return false;

	const previousMessageId = previous.payload?.message_id;
	const nextMessageId = next.payload?.message_id;
	return typeof previousMessageId === 'string'
		&& previousMessageId.length > 0
		&& typeof nextMessageId === 'string'
		&& nextMessageId.length > 0
		&& previousMessageId !== nextMessageId;
}

function isNearSimultaneous(previous: SessionEvent, next: SessionEvent): boolean {
	const previousTime = serverTimestampMs(previous.created_at);
	const nextTime = serverTimestampMs(next.created_at);
	return previousTime !== null
		&& nextTime !== null
		&& nextTime >= previousTime
		&& nextTime - previousTime <= MAX_FRAGMENT_GAP_MS;
}

function isClearContinuation(previous: string, next: string): boolean {
	const previousText = previous.trimEnd();
	const nextText = next.trimStart();
	if (!previousText || !nextText) return false;

	const previousEndsWord = /[\p{L}\p{N}]$/u.test(previousText);
	const previousEndsJoinableToken = /[\p{L}\p{N}\p{M}'\u2019"\u201d)}\]]$/u.test(previousText);
	const previousEndsContinuation = /[,;:([{\-/\u2010-\u2014]$/u.test(previousText);
	const nextStartsLowercase = /^\p{Ll}/u.test(nextText);
	const nextStartsClosingPunctuation = /^[,.;:!?%)}\]]/u.test(nextText);
	const nextStartsJoinedDash = /^[\u2010-\u2014-]\S/u.test(nextText);

	return (previousEndsJoinableToken && nextStartsClosingPunctuation)
		|| (previousEndsWord && nextStartsJoinedDash)
		|| ((previousEndsWord || previousEndsContinuation) && nextStartsLowercase);
}

function hasMessageBoundary(previous: SessionEvent, next: SessionEvent, messages: TaskMessage[]): boolean {
	const previousTime = serverTimestampMs(previous.created_at);
	const nextTime = serverTimestampMs(next.created_at);
	if (previousTime === null || nextTime === null) return true;

	return messages.some((message) => {
		const messageTime = serverTimestampMs(message.timestamp);
		if (messageTime === null) return false;
		const messageRank = message.role === 'user' ? 0 : 2;
		const followsPrevious = messageTime > previousTime || (messageTime === previousTime && messageRank > 1);
		const precedesNext = messageTime < nextTime || (messageTime === nextTime && messageRank < 1);
		return followsPrevious && precedesNext;
	});
}

function joinFragments(previous: string, next: string): string {
	const previousText = previous.trimEnd();
	const nextText = next.trimStart();
	const joinsWithoutSpace = /^[,.;:!?%)}\]]/u.test(nextText)
		|| (/^[\u2010-\u2014-]\S/u.test(nextText) && /[\p{L}\p{N}]$/u.test(previousText));
	return `${previousText}${joinsWithoutSpace ? '' : ' '}${nextText}`;
}

/**
 * Repair a rare Codex adapter presentation artifact where one grammatical
 * update arrives as several near-simultaneous ACP messages. The durable ACP
 * boundaries remain intact; only clearly continuous adjacent rows are folded.
 */
export function coalesceAgentMessageFragments(events: SessionEvent[], messages: TaskMessage[] = []): SessionEvent[] {
	const coalesced: SessionEvent[] = [];

	for (const event of events) {
		const previousIndex = coalesced.length - 1;
		const previous = coalesced[previousIndex];
		if (previous
			&& isAgentMessage(previous)
			&& isAgentMessage(event)
			&& isSameAgentMessageStream(previous, event)
			&& isNearSimultaneous(previous, event)
			&& !hasMessageBoundary(previous, event, messages)
			&& isClearContinuation(previous.summary, event.summary)) {
			coalesced[previousIndex] = {
				...previous,
				summary: joinFragments(previous.summary, event.summary),
				created_at: event.created_at,
			};
			continue;
		}

		coalesced.push(event);
	}

	return coalesced;
}
