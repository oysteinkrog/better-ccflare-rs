import { requestEvents } from "@better-ccflare/core";
import {
	sanitizeRequestHeaders,
	withSanitizedProxyHeaders,
} from "@better-ccflare/http-common";
import { ANALYTICS_STREAM_SYMBOL } from "@better-ccflare/http-common/symbols";
import type { Account } from "@better-ccflare/types";
import type { ProxyContext } from "./handlers";
import type { ChunkMessage, EndMessage, StartMessage } from "./worker-messages";

/**
 * Safely post a message to the worker, handling terminated workers
 */
function safePostMessage(
	worker: Worker,
	message: StartMessage | ChunkMessage | EndMessage,
): void {
	try {
		worker.postMessage(message);
	} catch (_error) {
		// Worker has been terminated, silently ignore
		// The error will be logged by the worker error handler in proxy.ts
	}
}

/**
 * Check if a response should be considered successful/expected
 * Treats certain well-known paths that return 404 as expected
 */
function isExpectedResponse(path: string, status: number): boolean {
	// Any .well-known path returning 404 is expected
	if (path.startsWith("/.well-known/") && status === 404) {
		return true;
	}

	// Otherwise use standard HTTP success logic
	return status >= 200 && status < 300;
}

export interface ResponseHandlerOptions {
	requestId: string;
	method: string;
	path: string;
	account: Account | null;
	requestHeaders: Headers;
	requestBody: ArrayBuffer | null;
	response: Response;
	timestamp: number;
	retryAttempt: number;
	failoverAttempts: number;
	agentUsed?: string | null;
	project?: string | null;
	apiKeyId?: string | null;
	apiKeyName?: string | null;
}

/**
 * Unified response handler that immediately streams responses
 * while forwarding data to worker for async processing
 */
// Forward response to client while streaming analytics to worker
export async function forwardToClient(
	options: ResponseHandlerOptions,
	ctx: ProxyContext,
): Promise<Response> {
	const {
		requestId,
		method,
		path,
		account,
		requestHeaders,
		requestBody,
		response: responseRaw,
		timestamp,
		retryAttempt, // Always 0 in new flow, but kept for message compatibility
		failoverAttempts,
		agentUsed,
		project,
		apiKeyId,
		apiKeyName,
	} = options;

	// Always strip compression headers *before* we do anything else
	const response = withSanitizedProxyHeaders(responseRaw);

	// Prepare objects once for serialisation - sanitize headers before storing
	const sanitizedReq = sanitizeRequestHeaders(requestHeaders);
	const requestHeadersObj = Object.fromEntries(sanitizedReq.entries());

	const responseHeadersObj = Object.fromEntries(response.headers.entries());

	const isStream = ctx.provider.isStreamingResponse?.(response) ?? false;

	// Filter out count_tokens requests for OpenAI-compatible providers from request logs and worker
	const shouldProcessRequest = !(
		ctx.provider.name === "openai-compatible" &&
		path === "/v1/messages/count_tokens"
	);

	// Send START message immediately if not filtered
	if (shouldProcessRequest) {
		// Cap request body to avoid sending large payloads (long conversation histories)
		// to the worker via postMessage. Bodies > 256KB are omitted.
		const MAX_REQUEST_BODY_BYTES = 256 * 1024;
		const requestBodyB64 =
			requestBody && requestBody.byteLength <= MAX_REQUEST_BODY_BYTES
				? Buffer.from(requestBody).toString("base64")
				: null;

		const startMessage: StartMessage = {
			type: "start",
			requestId,
			accountId: account?.id || null,
			method,
			path,
			timestamp,
			requestHeaders: requestHeadersObj,
			requestBody: requestBodyB64,
			responseStatus: response.status,
			responseHeaders: responseHeadersObj,
			isStream,
			providerName: ctx.provider.name,
			agentUsed: agentUsed || null,
			project: project || null,
			apiKeyId: apiKeyId || null,
			apiKeyName: apiKeyName || null,
			retryAttempt,
			failoverAttempts,
		};
		safePostMessage(ctx.usageWorker, startMessage);
	}

	// Emit request start event for real-time dashboard
	if (shouldProcessRequest) {
		requestEvents.emit("event", {
			type: "start",
			id: requestId,
			timestamp,
			method,
			path,
			accountId: account?.id || null,
			statusCode: response.status,
			agentUsed: agentUsed || null,
		});
	}

	const responseStatus = response.status;

	/*********************************************************************
	 *  STREAMING RESPONSES — tee the body stream for analytics
	 *  Avoids response.clone() which creates an internal tee buffer that
	 *  can leak memory when one branch is consumed slower than the other.
	 *********************************************************************/
	if (isStream && response.body) {
		// For OpenAI providers, use pre-teed analytics stream if available
		const preTeedStream = (response as any)[ANALYTICS_STREAM_SYMBOL];

		let clientStream: ReadableStream<Uint8Array>;
		let analyticsStream: ReadableStream<Uint8Array>;

		if (preTeedStream && preTeedStream instanceof ReadableStream) {
			// OpenAI provider already teed the stream
			clientStream = response.body;
			analyticsStream = preTeedStream;
		} else {
			// Tee the body stream directly — no response.clone() needed
			const [branch1, branch2] = response.body.tee();
			clientStream = branch1;
			analyticsStream = branch2;
		}

		// Read analytics branch in background, send chunks to worker
		if (shouldProcessRequest) {
			(async () => {
				const STREAM_TIMEOUT_MS = 300000; // 5 minutes max stream duration
				const CHUNK_TIMEOUT_MS = 30000; // 30 seconds between chunks

				try {
					const reader = analyticsStream.getReader();
					const startTime = Date.now();
					let lastChunkTime = Date.now();

					// eslint-disable-next-line no-constant-condition
					while (true) {
						// Check for overall stream timeout
						if (Date.now() - startTime > STREAM_TIMEOUT_MS) {
							await reader.cancel();
							throw new Error(
								`Stream timeout: exceeded ${STREAM_TIMEOUT_MS}ms total duration`,
							);
						}

						// Check for chunk timeout (no data received)
						if (Date.now() - lastChunkTime > CHUNK_TIMEOUT_MS) {
							await reader.cancel();
							throw new Error(
								`Stream timeout: no data received for ${CHUNK_TIMEOUT_MS}ms`,
							);
						}

						// Read with a timeout wrapper that properly cleans up
						const readPromise = reader.read();
						let timeoutId: Timer | null = null;
						const timeoutPromise = new Promise<{
							value?: Uint8Array;
							done: boolean;
						}>((_, reject) => {
							timeoutId = setTimeout(
								() => reject(new Error("Read operation timeout")),
								CHUNK_TIMEOUT_MS,
							);
						});

						try {
							const { value, done } = await Promise.race([
								readPromise,
								timeoutPromise,
							]);

							// Clear timeout if race completed successfully
							if (timeoutId) {
								clearTimeout(timeoutId);
								timeoutId = null;
							}

							if (done) break;

							if (value) {
								lastChunkTime = Date.now();
								const chunkMsg: ChunkMessage = {
									type: "chunk",
									requestId,
									data: value,
								};
								safePostMessage(ctx.usageWorker, chunkMsg);
							}
						} catch (error) {
							// Ensure timeout is cleared on error
							if (timeoutId) {
								clearTimeout(timeoutId);
								timeoutId = null;
							}
							throw error;
						}
					}
					// Finished without errors
					const endMsg: EndMessage = {
						type: "end",
						requestId,
						success: isExpectedResponse(path, responseStatus),
					};
					safePostMessage(ctx.usageWorker, endMsg);
				} catch (err) {
					const endMsg: EndMessage = {
						type: "end",
						requestId,
						success: false,
						error: (err as Error).message,
					};
					safePostMessage(ctx.usageWorker, endMsg);
				}
			})();
		} else {
			// Not processing this request — cancel the analytics branch to avoid leak
			analyticsStream.cancel().catch(() => {});
		}

		// Return new response with the client branch
		return new Response(clientStream, {
			status: response.status,
			statusText: response.statusText,
			headers: response.headers,
		});
	}

	/*********************************************************************
	 *  NON-STREAMING RESPONSES — read body once, send to worker
	 *  Avoids response.clone() by reading arrayBuffer and creating a
	 *  new Response from it for the client.
	 *********************************************************************/
	const bodyBuf = await response.arrayBuffer();

	if (shouldProcessRequest) {
		// Cap non-streaming response body sent to worker at 256KB
		const MAX_RESPONSE_BODY_BYTES = 256 * 1024;
		const endMsg: EndMessage = {
			type: "end",
			requestId,
			responseBody:
				bodyBuf.byteLength > 0 && bodyBuf.byteLength <= MAX_RESPONSE_BODY_BYTES
					? Buffer.from(bodyBuf).toString("base64")
					: null,
			success: isExpectedResponse(path, responseStatus),
		};
		safePostMessage(ctx.usageWorker, endMsg);
	}

	// Return new response from the already-read body
	return new Response(bodyBuf, {
		status: response.status,
		statusText: response.statusText,
		headers: response.headers,
	});
}
