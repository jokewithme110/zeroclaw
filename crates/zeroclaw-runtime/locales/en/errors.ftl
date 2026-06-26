err-provider-auth-failed = ⚠️ AI service connection failed: API key is invalid

    The API key for { $provider } could not be verified.

    Possible reasons:
    • The API key was copied incorrectly
    • The API key was deleted or regenerated on the provider side
    • The API key has expired
    • The provider account is suspended because billing is overdue

   👉 What to do:
    • Sign in to the official website of { $provider } and check whether the API key is still valid
    • If it has expired, generate a new API key
    • Update the API key in your configuration and make sure there are no extra spaces

err-provider-rate-limited = ⚠️ Too many requests. Please wait a moment

    Too many messages were sent in a short time, so { $provider } has temporarily limited requests.

   👉 What to do:
    • Wait 30 to 60 seconds, then send the message again
    • If this happens often, the current plan may not provide enough request capacity

err-provider-quota-exceeded = ⚠️ AI service quota is not enough

    Your balance or free quota on { $provider } has been used up.

   👉 What to do:
    • Sign in to { $provider } and check your balance and usage
    • If the balance is low, recharge and try again
    • If the free quota is used up, wait for it to reset or buy more quota
    • You can also switch to another provider and continue using the service

err-provider-model-not-found = ⚠️ The AI model name is not valid

    The model "{ $model }" is not available on { $provider }.

    Possible reasons:
    • The model name was entered incorrectly
    • The model was renamed or removed by the provider
    • Your account does not have access to this model

   👉 What to do:
    • Check whether the model name is correct
    • Review the model list on { $provider }
    • Update the configuration with a valid model name

err-provider-vision-not-supported = ⚠️ The current AI model cannot read images

    You sent an image, but the current model "{ $model }" does not support image understanding.

   👉 What to do:
    • If your question is text-only, send text without the image
    • If you need image analysis, switch to a vision-capable model

err-provider-context-window-exceeded = ⚠️ The conversation is too long for the AI to handle

    The current conversation has grown beyond what model "{ $model }" can process in one request.

   👉 What to do:
    • Clear the conversation context and ask again
    • Split a large question into several smaller questions
    • If this happens often, reduce the amount of chat history kept for each session

err-provider-context-window-compacted = ⚠️ The conversation is too long for the AI to handle

    I compacted recent history and kept the latest context.

   👉 What to do:
    • Please resend your last message
    • If it still fails, split the question into smaller parts

err-provider-timeout = ⚠️ The AI response timed out

    The AI model did not return a result in time.

    Possible reasons:
    • The model is under heavy load
    • The network is slow
    • The question is complex and needs more time to process

   👉 What to do:
    • Wait a moment and try again
    • Try a simpler question first
    • If this happens often, increase the timeout or switch to a faster model

err-provider-network-error = ⚠️ Network connection failed

    The current network cannot reach { $provider }.

    Possible reasons:
    • The current network connection is unstable
    • The provider is temporarily unreachable from this network
    • DNS resolution failed

   👉 What to do:
    • Confirm that the device can access the internet normally
    • Wait a few minutes and try again
    • If it keeps failing, try another provider or restart the network device

err-provider-server-error = ⚠️ { $provider } is temporarily unavailable

    This is not caused by your local setup. The AI provider is currently having a server-side problem.

   👉 What to do:
    • Wait 1 to 2 minutes and try again
    • If it lasts for a long time, check whether { $provider } has posted an outage notice
    • If you need an immediate reply, switch to another provider for now

err-provider-unknown = ⚠️ Service error. Please try again later

    If this keeps happening, try the following:
    • Wait a few seconds and send the message again
    • Check whether your settings are complete and correct
    • Restart the device
    • If the problem still does not go away, contact product support
