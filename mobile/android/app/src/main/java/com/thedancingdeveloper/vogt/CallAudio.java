package com.thedancingdeveloper.vogt;

import android.content.Context;
import android.media.AudioDeviceInfo;
import android.media.AudioManager;

/**
 * The phone's audio mode for a live assistant call (WI-960).
 *
 * <p>A call needs the platform's communication mode: in it, Android routes the
 * microphone through the device's acoustic echo canceller and noise suppressor,
 * which is what lets the user talk over the reply without the reply's own echo
 * interrupting it. The reply goes to the loudspeaker unless a headset is
 * connected (then it stays on the headset).
 *
 * <p>Whatever this changes, it records first and restores on {@link #end}; the
 * PWA starts and ends the mode ({@code call-start} / {@code call-end} over the
 * voice bridge), and the activity ends it if it is destroyed mid-call. Starting
 * twice is one start; ending without a start changes nothing.
 */
final class CallAudio {
    /** The slice of {@link AudioManager} this uses, so the logic is testable. */
    interface Device {
        int mode();

        void setMode(int mode);

        boolean speakerphone();

        void setSpeakerphone(boolean on);

        boolean headsetConnected();
    }

    private final Device device;
    private boolean active;
    private int savedMode;
    private boolean savedSpeakerphone;

    CallAudio(Device device) {
        this.device = device;
    }

    /** Enter call mode, remembering what to restore. */
    synchronized void start() {
        if (active) {
            return;
        }
        savedMode = device.mode();
        savedSpeakerphone = device.speakerphone();
        device.setMode(AudioManager.MODE_IN_COMMUNICATION);
        device.setSpeakerphone(!device.headsetConnected());
        active = true;
    }

    /** Leave call mode, restoring exactly what {@link #start} changed. */
    synchronized void end() {
        if (!active) {
            return;
        }
        device.setSpeakerphone(savedSpeakerphone);
        device.setMode(savedMode);
        active = false;
    }

    synchronized boolean active() {
        return active;
    }

    private static CallAudio shared;

    /** The process-wide instance over the real {@link AudioManager}. */
    static synchronized CallAudio shared(Context context) {
        if (shared == null) {
            AudioManager manager =
                (AudioManager) context.getApplicationContext().getSystemService(Context.AUDIO_SERVICE);
            shared = new CallAudio(new SystemDevice(manager));
        }
        return shared;
    }

    /** {@link Device} over the platform audio manager. */
    static final class SystemDevice implements Device {
        private final AudioManager manager;

        SystemDevice(AudioManager manager) {
            this.manager = manager;
        }

        @Override
        public int mode() {
            return manager.getMode();
        }

        @Override
        public void setMode(int mode) {
            manager.setMode(mode);
        }

        // Deprecated at API 31 in favour of setCommunicationDevice, but still
        // honoured in MODE_IN_COMMUNICATION, and it is the one call that works
        // from minSdk 24 up.
        @SuppressWarnings("deprecation")
        @Override
        public boolean speakerphone() {
            return manager.isSpeakerphoneOn();
        }

        @SuppressWarnings("deprecation")
        @Override
        public void setSpeakerphone(boolean on) {
            manager.setSpeakerphoneOn(on);
        }

        @Override
        public boolean headsetConnected() {
            for (AudioDeviceInfo info : manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
                switch (info.getType()) {
                    case AudioDeviceInfo.TYPE_WIRED_HEADSET:
                    case AudioDeviceInfo.TYPE_WIRED_HEADPHONES:
                    case AudioDeviceInfo.TYPE_BLUETOOTH_SCO:
                    case AudioDeviceInfo.TYPE_BLUETOOTH_A2DP:
                    case AudioDeviceInfo.TYPE_USB_HEADSET:
                        return true;
                    default:
                        break;
                }
            }
            return false;
        }
    }
}
