import { countdownParts } from "./countdown.ts"

let checks = 0

function equal(actual: unknown, expected: unknown) {
  checks++
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`)
  }
}

function ok(condition: unknown, message: string) {
  checks++
  if (!condition) throw new Error(message)
}

equal(countdownParts(2, 0), [2, 0])
equal(countdownParts(2, 1), [1, 59])
equal(countdownParts(2, 59), [1, 1])
equal(countdownParts(2, 60), [1, 0])
equal(countdownParts(1345, 60, false), [1345, 0])

let previous = Infinity
for (let elapsed = 0; elapsed <= 121; elapsed++) {
  const [minutes, seconds] = countdownParts(3, elapsed)
  ok(seconds >= 0 && seconds <= 59, `seconds=${seconds}`)
  const total = minutes * 60 + seconds
  ok(total <= previous, `elapsed=${elapsed}`)
  previous = total
}

console.log(`countdown checks passed (${checks} assertions)`)
