package com.thedancingdeveloper.vogt;

final class NativeInsets {
    private NativeInsets() {}

    static int toCssPixels(int physicalPixels, float density) {
        if (density <= 0) {
            return physicalPixels;
        }
        return Math.round(physicalPixels / density);
    }
}
