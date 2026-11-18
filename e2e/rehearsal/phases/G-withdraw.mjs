// Phase G: withdrawal with a proof paid by the vault. Not built yet: recorded as blocked, never as passed. UNAUDITED prototype.
export default async function (ctx) {
  await ctx.blocked('G-01', "withdrawal with a proof paid by the vault", "needs the vault on anvil and the node's withdrawal certificate (431 task 3)");
}
