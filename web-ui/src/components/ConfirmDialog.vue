<template>
  <!--
    The service requires `X-Confirm-Destructive: yes` on stop and retry-all
    precisely so that a client asks the operator first. The UI set the header
    automatically and acted on a single click, which on a wall monitor during a
    bulk ingest is one mis-click away from killing every in-flight encode.

    Multi-tier confirmation per NOTEBOOKLM_PLAYOUT_UI_UX §1(5): a distinct
    danger colour, and the cancel button focused rather than the dangerous one.
  -->
  <Teleport to="body">
    <div v-if="open" class="cd-backdrop" @click.self="$emit('cancel')">
      <div
        class="cd-panel"
        role="alertdialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        :aria-describedby="bodyId"
        @keydown.esc="$emit('cancel')"
      >
        <h2 :id="titleId" class="cd-title">{{ title }}</h2>
        <p :id="bodyId" class="cd-body">{{ body }}</p>
        <p v-if="detail" class="cd-detail">{{ detail }}</p>
        <div class="cd-actions">
          <button ref="cancelRef" class="btn cd-cancel" @click="$emit('cancel')">Cancel</button>
          <button class="btn cd-confirm" :disabled="busy" @click="$emit('confirm')">
            {{ busy ? 'Working…' : confirmLabel }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { ref, watch, nextTick, useId } from 'vue'

const props = defineProps<{
  open: boolean
  title: string
  body: string
  /** Second line, for the consequence an operator may not have in mind. */
  detail?: string
  confirmLabel: string
  busy?: boolean
}>()

defineEmits<{
  confirm: []
  cancel: []
}>()

const titleId = useId()
const bodyId = useId()
const cancelRef = ref<HTMLButtonElement | null>(null)

// Focus lands on Cancel, never on the destructive button: a stray Return must
// not be the thing that stops a bulk ingest.
watch(
  () => props.open,
  async (open) => {
    if (!open) return
    await nextTick()
    cancelRef.value?.focus()
  },
)
</script>

<style scoped>
.cd-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.cd-panel {
  background: var(--bg-panel);
  border: 1px solid var(--accent-crimson);
  border-radius: var(--radius-base);
  padding: 20px 22px;
  max-width: 460px;
  box-shadow: 0 16px 48px rgba(0, 0, 0, 0.55);
}

.cd-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--accent-crimson);
  margin-bottom: 10px;
}

.cd-body {
  font-size: 13px;
  line-height: 1.55;
  color: var(--text-primary);
}

.cd-detail {
  font-size: 12px;
  line-height: 1.55;
  color: var(--text-secondary);
  margin-top: 8px;
}

.cd-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  margin-top: 18px;
}

.cd-cancel {
  padding: 6px 18px;
  font-size: 13px;
}

.cd-confirm {
  padding: 6px 18px;
  font-size: 13px;
  background: var(--accent-crimson);
  border-color: var(--accent-crimson);
  color: #fff;
  font-weight: 600;
}

.cd-confirm:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}
</style>
