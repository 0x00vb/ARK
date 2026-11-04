import { h } from "./dom.js";

let reviewing = false;
export async function reviewComputerMessage(call, completed, trigger = document.activeElement) {
  if (reviewing) return;
  reviewing = true;
  let review;
  try { review = await call("computer_review"); }
  catch (failure) { reviewing = false; throw failure; }
  const before = trigger;
  const cancel = h("button.btn.ghost.small", { type: "button", autofocus: true }, "Keep blocked");
  const accept = h("button.btn.small", { type: "button" }, "I checked the app");
  const error = h("p.computer-recovery-error", { role: "alert", hidden: true });
  const description = review.kind === "submitted_unconfirmed"
    ? `The message may already have been sent. Check ${review.app} for the sent message and any unfinished draft before continuing.`
    : `Clear any unwanted draft in ${review.app}. Continue after checking the conversation yourself.`;
  const dialog = h("dialog.computer-recovery", { "aria-labelledby": "computer-recovery-title", "aria-describedby": "computer-recovery-description" },
    h("h2", { id: "computer-recovery-title" }, "Allow new drafts?"),
    h("p", { id: "computer-recovery-description" }, description), error,
    h("div.approval-actions", cancel, accept));
  const close = () => { reviewing = false; dialog.close(); dialog.remove(); if (before?.isConnected) before.focus(); };
  cancel.onclick = close;
  dialog.addEventListener("cancel", event => { event.preventDefault(); if (!cancel.disabled) close(); });
  accept.onclick = async () => {
    accept.disabled = true;
    cancel.disabled = true;
    error.hidden = true;
    try {
      const result = await call("computer_acknowledge", { token: review.token, accepted_unverified: true });
      close();
      completed(result);
    } catch (failure) {
      error.textContent = failure.message;
      error.hidden = false;
      cancel.disabled = false;
      cancel.focus();
    }
  };
  document.body.append(dialog);
  dialog.showModal();
  cancel.focus();
}
