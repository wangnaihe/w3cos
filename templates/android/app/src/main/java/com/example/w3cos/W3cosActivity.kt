package com.example.w3cos

import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity

/**
 * RN-like shell Activity — loads libw3cos_mobile.so and starts the AOT-compiled app.
 *
 * M1: expects jniLibs/arm64-v8a/libw3cos_mobile.so (built via cargo-ndk).
 * M2: w3cos mobile build copies artifacts automatically.
 */
class W3cosActivity : AppCompatActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        System.loadLibrary("w3cos_mobile")
        Thread {
            val code = nativeRun("")
            if (code != 0) {
                runOnUiThread {
                    throw RuntimeException("w3cos_mobile_run exited with code $code")
                }
            }
        }.start()
    }

    private external fun nativeRun(manifestPath: String): Int
}
