package com.sprooty.vogt;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class NativeInsetsTest {
    @Test
    public void convertsPhysicalPixelsToCssPixels() {
        assertEquals(40, NativeInsets.toCssPixels(120, 3.0f));
        assertEquals(24, NativeInsets.toCssPixels(60, 2.5f));
    }

    @Test
    public void preservesPixelsWhenDensityIsUnavailable() {
        assertEquals(30, NativeInsets.toCssPixels(30, 0.0f));
    }
}
