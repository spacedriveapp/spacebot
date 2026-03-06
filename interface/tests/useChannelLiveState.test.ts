import { expect, test } from "bun:test";

import type { ChannelLiveState } from "../src/hooks/useChannelLiveState";

(globalThis as typeof globalThis & {
	window?: { __SPACEBOT_BASE_PATH?: string };
}).window = {
	__SPACEBOT_BASE_PATH: "",
};

const { applyOutboundMessageDelta, applyOutboundStreamEnd } = await import(
	"../src/hooks/useChannelLiveState"
);

function baseLiveState(): ChannelLiveState {
	return {
		isTyping: true,
		timeline: [],
		workers: {},
		branches: {},
		streamingMessageId: null,
		historyLoaded: true,
		hasMore: false,
		loadingMore: false,
	};
}

test("outbound stream end clears the active placeholder before the next stream", () => {
	const afterFirstDelta = applyOutboundMessageDelta(baseLiveState(), {
		type: "outbound_message_delta",
		agent_id: "agent-1",
		channel_id: "channel-1",
		text_delta: "partial",
		aggregated_text: "partial",
	});

	expect(afterFirstDelta.streamingMessageId).not.toBeNull();
	expect(afterFirstDelta.timeline).toHaveLength(1);

	const firstStreamMessageId = afterFirstDelta.streamingMessageId;
	const afterStreamEnd = applyOutboundStreamEnd(afterFirstDelta);

	expect(afterStreamEnd.streamingMessageId).toBeNull();
	expect(afterStreamEnd.isTyping).toBe(false);
	expect(afterStreamEnd.timeline).toHaveLength(0);

	const afterNextDelta = applyOutboundMessageDelta(afterStreamEnd, {
		type: "outbound_message_delta",
		agent_id: "agent-1",
		channel_id: "channel-1",
		text_delta: "new reply",
		aggregated_text: "new reply",
	});

	expect(afterNextDelta.streamingMessageId).not.toBeNull();
	expect(afterNextDelta.streamingMessageId).not.toBe(firstStreamMessageId);
	expect(afterNextDelta.timeline).toHaveLength(1);
	expect(afterNextDelta.timeline[0]).toMatchObject({
		type: "message",
		content: "new reply",
	});
});

test("outbound stream end without a placeholder still clears typing", () => {
	const updated = applyOutboundStreamEnd(baseLiveState());

	expect(updated.streamingMessageId).toBeNull();
	expect(updated.isTyping).toBe(false);
	expect(updated.timeline).toHaveLength(0);
});
