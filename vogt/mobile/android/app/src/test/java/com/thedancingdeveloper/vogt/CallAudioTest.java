package com.thedancingdeveloper.vogt;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import android.media.AudioManager;

import org.junit.Test;

public class CallAudioTest {
    static final class FakeDevice implements CallAudio.Device {
        int mode = AudioManager.MODE_NORMAL;
        boolean speaker = false;
        boolean headset = false;
        int modeChanges = 0;

        @Override public int mode() { return mode; }
        @Override public void setMode(int mode) { this.mode = mode; modeChanges++; }
        @Override public boolean speakerphone() { return speaker; }
        @Override public void setSpeakerphone(boolean on) { speaker = on; }
        @Override public boolean headsetConnected() { return headset; }
    }

    @Test
    public void a_call_uses_communication_mode_on_the_loudspeaker_and_restores_both() {
        FakeDevice device = new FakeDevice();
        CallAudio audio = new CallAudio(device);
        audio.start();
        assertEquals(AudioManager.MODE_IN_COMMUNICATION, device.mode);
        assertTrue(device.speaker);
        audio.end();
        assertEquals(AudioManager.MODE_NORMAL, device.mode);
        assertFalse(device.speaker);
        assertFalse(audio.active());
    }

    @Test
    public void a_headset_keeps_the_reply_off_the_loudspeaker() {
        FakeDevice device = new FakeDevice();
        device.headset = true;
        new CallAudio(device).start();
        assertFalse(device.speaker);
        assertEquals(AudioManager.MODE_IN_COMMUNICATION, device.mode);
    }

    @Test
    public void starting_twice_is_one_start_and_ending_unstarted_changes_nothing() {
        FakeDevice device = new FakeDevice();
        device.mode = AudioManager.MODE_RINGTONE;
        CallAudio audio = new CallAudio(device);
        audio.end();
        assertEquals(0, device.modeChanges);
        audio.start();
        audio.start();
        audio.end();
        // Restored to what it was before the *first* start, not to call mode.
        assertEquals(AudioManager.MODE_RINGTONE, device.mode);
    }
}
