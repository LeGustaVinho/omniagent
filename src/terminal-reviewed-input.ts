let pending: { target: string; checkoutId: string; text: string } | undefined;
const prefix = "omni-terminal-review:";

export function reviewTerminalInputTarget(checkoutId: string, text: string) {
  const target = prefix + crypto.randomUUID();
  pending = { target, checkoutId, text };
  return target;
}

export function isTerminalReviewTarget(target?: string) {
  return target?.startsWith(prefix) ?? false;
}

export function takeTerminalReview(target: string, checkoutId: string) {
  if (pending?.target !== target) return;
  const request = pending;
  pending = undefined;
  return request.checkoutId === checkoutId ? request : undefined;
}
