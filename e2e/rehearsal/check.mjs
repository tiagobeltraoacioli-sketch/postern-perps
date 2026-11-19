// Check vocabulary of the rehearsal. UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
/** Thrown by a check that could not run: recorded as `blocked` with the reason, never as passed. */
export class Blocked extends Error {}
export const expect = (cond, msg) => { if (!cond) throw new Error(msg); };
export const blockedIf = (cond, reason) => { if (cond) throw new Blocked(reason); };
