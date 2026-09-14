/** Remaining time in whole minutes and seconds, counting down from a fresh progress minute. */
export function countdownParts(totalMinutes: number, elapsedSeconds: number, counting = true): [number, number] {
  const remaining = Math.max(
    0,
    Math.round(totalMinutes * 60) - (counting ? Math.max(0, Math.floor(elapsedSeconds)) : 0),
  )
  return [Math.floor(remaining / 60), remaining % 60]
}
