# Speech

XpressClaw provides dictation and read-aloud independently of the Agent's
harness. No speech model needs to be embedded in a runner. Speech is disabled
until configured for the instance.

## Set up an audio provider

Open **Settings → Speech**, enable speech, and enter:

- **API base URL**: the provider's API prefix, such as
  `https://api.openai.com/v1` or `http://localhost:8000/v1`.
- **API key**: the provider's key, or leave it empty for a local service without
  authentication. An empty field preserves an existing key; **Remove saved key**
  clears it when you save.
- **Transcription model**, **Speech model**, and **Voice**: names accepted by
  your provider. The initial values are `whisper-1`, `tts-1`, and `alloy`.

Save the settings; no runner update or restart is needed. The endpoint must
support both OpenAI audio routes. A service compatible only with chat
completions will not work for speech. XpressClaw sends:

| Operation | Request |
| --- | --- |
| Transcription | `POST <base>/audio/transcriptions`, multipart `file`, `model`, `response_format=json`; expects `{ "text": "..." }` |
| Read aloud | `POST <base>/audio/speech`, JSON `model`, `voice`, `input`, `response_format=mp3`; expects audio bytes |

Requests originate from the XpressClaw server, so a local provider must be
reachable from that server. `localhost` refers to the server's host/container,
not to a remote browser or an Agent container. An API key, when set, is sent as
a Bearer token. Redirects are not followed.

## Use speech

Press the microphone button in a conversation, task, Agent, or new-work
composer. Grant microphone permission, speak, and press **Finish dictation**.
The transcript is appended to the current draft; review or edit it before
sending. **Cancel dictation** discards the recording or pending transcript.
Recordings stop after five minutes and must fit within 25 MiB.

Press **Read aloud** beside an Agent reply to hear an AI-generated voice.
Press it again to stop. Long replies are read in successive chunks; fenced code
and embedded visualizations are skipped. Starting another reply stops the
previous playback. Speech never starts automatically.

Recordings and text are sent to the configured provider. Raw recordings and
generated audio are not saved by XpressClaw; the transcript is saved when sent,
like any other message. Browser draft persistence also applies to dictated
text. The provider's own retention and billing policies apply.

## Storage and troubleshooting

Instance settings and the API key are stored in
`<system.data_dir>/speech-settings.json`, outside Project synchronization. The
file is atomically replaced with owner-only permissions on Unix. The key is
stored as plaintext in that restricted file, never returned by the settings API,
and never passed to an Agent harness.

Microphone recording requires a supported browser with microphone permission
and a secure context (HTTPS or localhost). Remote HTTP connections cannot use
browser dictation. Chromium normally records WebM/Opus; Safari records MP4, so
the provider must accept the browser's format. The macOS Desktop bundle includes
the microphone usage description and audio-input entitlement; grant access in
macOS settings if it was previously denied.

Provider errors appear beside the microphone or read-aloud control. A 401
usually indicates an invalid key; a 404 may indicate an incorrect base URL or
unsupported audio route. Check the configured model and voice names for other
provider errors. Requests time out after two minutes, and failed dictation
preserves the text already in your draft.
