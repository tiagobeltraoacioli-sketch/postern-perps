// Phase K: validators stop, escape, claims sum to custody with insurance first. Not built yet: recorded as blocked, never as passed. UNAUDITED prototype.
export default async function (ctx) {
  await ctx.blocked('K-01', "validators stop, escape, claims sum to custody with insurance first", "needs the vault on anvil and account proofs from the node (431 task 3)");
}
