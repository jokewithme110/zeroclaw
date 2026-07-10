err-provider-auth-failed = ⚠️ AI service connection failed: API key is invalid

    The API key for { $provider } could not be verified.

   👉 Possible reasons and what to do:
    • Check whether the API key was entered incorrectly, and sign in to { $provider } to confirm it is still valid
    • Check whether the API key has expired; if so, generate a new API key on { $provider }
    • The provider account may also be suspended because billing is overdue; recharge and try again

err-provider-rate-limited = ⚠️ Too many requests. Please wait a moment

    Too many requests were sent in a short time, so { $provider } has temporarily rejected this request.

   👉 Possible reasons and what to do:
    • The request volume was too high in a short time. Wait 30 to 60 seconds and try again
    • If this happens often, the current plan may have a low request-rate limit. Review the plan limits and upgrade if needed

err-provider-quota-exceeded = ⚠️ AI service quota is not enough

    Your account balance on { $provider } has been used up.

   👉 Possible reasons and what to do:
    • Your account balance may be too low. Sign in to { $provider } and check the current balance, then recharge if needed
    • The free quota may be exhausted. Check the reset policy on { $provider }, wait for reset, or buy more quota
    • If that still does not help, switch to another provider API key and continue using the service

err-provider-model-not-found = ⚠️ The AI model name is not valid

    The model "{ $model }" is not available on { $provider }.

   👉 Possible reasons and what to do:
    • The model name may be incorrect. Check spelling, letter case, and hyphens
    • The model may have been renamed or removed. Review the available model list on { $provider }
    • Your account may not have access to this model. Enable access and try again

err-provider-vision-not-supported = ⚠️ The current AI model cannot read images

    You sent an image, but the selected { $provider } model "{ $model }" does not support image understanding.

   👉 Possible reasons and what to do:
    • The current model is text-only. If you only need text chat, send a text message without the image
    • If you need image analysis, switch to a multimodal or vision-capable model and review the model capabilities on { $provider }

err-provider-context-window-compacted = ⚠️ The conversation is too long for the AI to handle

    The conversation became too long, so I compacted recent history and kept the latest context.

   👉 Possible reasons and what to do:
    • Please resend your last message
    • If it still fails, split the request into smaller questions

err-provider-context-window-exceeded = ⚠️ The conversation is too long for the AI to handle

    The accumulated conversation has exceeded the context window supported by the current { $provider } model.

   👉 Possible reasons and what to do:
    • The conversation history is too long. Clear the conversation context and ask again
    • The current question may be too complex or too long. Break it into smaller requests
    • If this happens often, reduce the amount of chat history kept, or switch to a model with a longer context window

err-provider-timeout = ⚠️ The AI response timed out

    The AI model did not return a result in time.

   👉 Possible reasons and what to do:
    • The current model may be under heavy load. Wait a moment and try again
    • If repeated retries still fail, { $provider } may be overloaded. Wait for recovery or switch to another provider

err-provider-network-error = ⚠️ Network connection failed

    The current network cannot reach the { $provider } server.

   👉 Possible reasons and what to do:
    • The current network connection may be unstable. Confirm that the device can access the internet normally
    • The provider may be temporarily unreachable from this network. Consider switching to a domestic provider if applicable
    • DNS resolution may have failed. Restart the network device and try again

err-provider-server-error = ⚠️ The { $provider } server is temporarily down

    This is not caused by your configuration. The problem is on the { $provider } server side.

   👉 Possible reasons and what to do:
    • The { $provider } server may be overloaded, restarting, or under maintenance. Wait 1 to 2 minutes and try again
    • If it still fails after more than 10 minutes, check the service status page or notices from { $provider }
    • If you need an immediate reply, switch to another provider API key for now

err-provider-unknown = ⚠️ Service error. Please try again later

    The request could not be completed. This may be caused by network fluctuation, local configuration issues, or a temporary problem on the { $provider } side.

   👉 Possible reasons and what to do:
    • The network may be unstable or the request may have timed out. Wait a few seconds and try again
    • The API configuration may be incomplete or incorrect. Check whether all fields are filled in correctly
    • The local client or device state may be abnormal. Restart it and try again
    • If none of the above helps, contact product support or { $provider } support for further investigation
