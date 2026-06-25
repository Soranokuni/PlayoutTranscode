<template>
  <div class="progress-wrapper">
    <div class="progress-outer">
      <div
        class="progress-inner"
        :class="{ indeterminate: !determinate, complete: percent >= 100 }"
        :style="determinate ? { width: clampPercent + '%' } : {}"
      />
    </div>
    <div class="progress-details">
      <span v-if="determinate && Number.isFinite(percent)" class="progress-pct">{{ Math.round(percent) }}%</span>
      <span v-else class="progress-pct indeterminate-label">Analyzing</span>
      <span v-if="eta" class="progress-eta">{{ eta }}</span>
      <span v-if="speedText" class="progress-speed">{{ speedText }}</span>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  percent: number
  determinate: boolean
  speed?: string
  durationMs?: number
  currentTimeMs?: number
}>()

const clampPercent = computed(() => Math.min(100, Math.max(0, props.percent)))

const speedText = computed(() => {
  if (!props.speed || props.speed === '0x') return ''
  return props.speed
})

const eta = computed(() => {
  if (!props.determinate || props.percent <= 0 || props.percent >= 100) return ''
  const speedNumeric = parseFloat(props.speed || '1')
  if (!Number.isFinite(speedNumeric) || speedNumeric <= 0) return ''
  if (props.durationMs && props.currentTimeMs && props.durationMs > 0) {
    const remaining = (props.durationMs - props.currentTimeMs) / speedNumeric
    if (remaining <= 0) return ''
    const totalSecs = Math.round(remaining / 1000)
    if (totalSecs < 60) return `${totalSecs}s left`
    const mins = Math.floor(totalSecs / 60)
    const secs = totalSecs % 60
    return `${mins}:${secs.toString().padStart(2, '0')} left`
  }
  return ''
})
</script>

<style scoped>
.progress-wrapper {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.progress-outer {
  width: 140px;
  height: 6px;
  background: rgba(255,255,255,0.06);
  border-radius: 3px;
  overflow: hidden;
  flex-shrink: 0;
}

.progress-inner {
  height: 100%;
  background: var(--accent-cyan);
  border-radius: 3px;
  transition: width 300ms ease-in-out;
}

.progress-inner.complete {
  background: var(--accent-emerald);
}

.progress-inner.indeterminate {
  width: 30%;
  animation: progressPulse 1.4s ease-in-out infinite;
  background: var(--accent-cyan);
}

@keyframes progressPulse {
  0% { margin-left: 0; width: 25%; }
  50% { margin-left: 35%; width: 30%; }
  100% { margin-left: 75%; width: 25%; }
}

.progress-details {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
  white-space: nowrap;
  flex-shrink: 0;
}

.progress-pct {
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  color: var(--accent-cyan);
  min-width: 32px;
}

.progress-pct.indeterminate-label {
  color: var(--text-secondary);
}

.progress-eta {
  color: var(--text-secondary);
}

.progress-speed {
  color: var(--accent-cyan);
  font-weight: 500;
}
</style>
