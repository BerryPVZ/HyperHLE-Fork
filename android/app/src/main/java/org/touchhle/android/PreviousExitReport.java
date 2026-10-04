/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
package org.touchhle.android;

import android.app.ActivityManager;
import android.app.ApplicationExitInfo;
import android.content.Context;
import android.os.Build;
import android.util.AtomicFile;
import android.util.Base64;
import android.util.Base64OutputStream;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

/** Recover OS crash evidence after a native failure bypasses our log handler. */
final class PreviousExitReport {
    private static final int TRACE_LIMIT = 8 * 1024 * 1024;

    static void collect(Context context) {
        if (Build.VERSION.SDK_INT < 30) {
            return;
        }
        final Context app = context.getApplicationContext();
        final long launchTime = System.currentTimeMillis();
        new Thread(() -> collectOnWorker(app, launchTime), "previous-exit-report").start();
    }

    private static void collectOnWorker(Context app, long launchTime) {
        try {
            ActivityManager manager = (ActivityManager) app.getSystemService(Context.ACTIVITY_SERVICE);
            File directory = app.getExternalFilesDir(null);
            if (manager == null || directory == null) {
                return;
            }
            ApplicationExitInfo latest = null;
            for (ApplicationExitInfo exit : manager.getHistoricalProcessExitReasons(
                    app.getPackageName(), 0, 16)) {
                int reason = exit.getReason();
                if (exit.getTimestamp() >= launchTime
                        || !(reason == ApplicationExitInfo.REASON_CRASH_NATIVE
                        || reason == ApplicationExitInfo.REASON_CRASH
                        || reason == ApplicationExitInfo.REASON_LOW_MEMORY
                        || reason == ApplicationExitInfo.REASON_SIGNALED
                        || reason == ApplicationExitInfo.REASON_ANR)) {
                    continue;
                }
                if (latest == null || exit.getTimestamp() > latest.getTimestamp()) {
                    latest = exit;
                }
            }
            if (latest == null) {
                return;
            }
            // Keep the last report across clean launches. Never overwrite the
            // emulator log: SDL/Rust may already be creating that file.
            File reportFile = new File(directory, "touchHLE_last_crash.txt");
            android.content.SharedPreferences saved = app.getSharedPreferences(
                    "previous_exit_report", Context.MODE_PRIVATE);
            if (reportFile.isFile() && saved.getLong("timestamp", -1) == latest.getTimestamp()) {
                return;
            }
            AtomicFile report = new AtomicFile(reportFile);
            FileOutputStream output = report.startWrite();
            try {
                write(output, "Android previous-process exit report\n"
                        + "Package: " + app.getPackageName() + "\n"
                        + "Process: " + latest.getProcessName() + "\n"
                        + "Timestamp (epoch ms): " + latest.getTimestamp() + "\n"
                        + "Reason: " + latest.getReason() + "\n"
                        + "Status/signal: " + latest.getStatus() + "\n"
                        + "Description: " + latest.getDescription() + "\n"
                        + "Last sampled PSS (KiB): " + latest.getPss() + "\n"
                        + "Last sampled RSS (KiB): " + latest.getRss() + "\n"
                        + "Android API: " + Build.VERSION.SDK_INT + "\n"
                        + "Device: " + Build.MANUFACTURER + " " + Build.MODEL + "\n");
                // API 31+ can supply a native tombstone protobuf. Preserve the
                // original bytes as base64 rather than misreading it as text.
                try (InputStream trace = latest.getTraceInputStream()) {
                    if (trace == null) {
                        write(output, "OS trace unavailable (Android may have expired it).\n");
                    } else {
                        write(output, "BEGIN OS TRACE BASE64\n");
                        int total = 0;
                        byte[] buffer = new byte[8192];
                        try (Base64OutputStream encoded = new Base64OutputStream(
                                output, Base64.NO_CLOSE)) {
                            while (total < TRACE_LIMIT) {
                                int count = trace.read(buffer, 0, Math.min(buffer.length, TRACE_LIMIT - total));
                                if (count < 0) break;
                                encoded.write(buffer, 0, count);
                                total += count;
                            }
                        }
                        write(output, "\nEND OS TRACE BASE64\n");
                        if (total == TRACE_LIMIT && trace.read() != -1) {
                            write(output, "Trace truncated at 8 MiB.\n");
                        }
                    }
                } catch (IOException error) {
                    write(output, "Could not read OS trace: " + error + "\n");
                }
                report.finishWrite(output);
                saved.edit().putLong("timestamp", latest.getTimestamp()).apply();
            } catch (Exception error) {
                report.failWrite(output);
                throw error;
            }
        } catch (Exception error) {
            // Diagnostics must never prevent the emulator from starting.
            Log.w("touchHLE", "Could not recover previous exit report", error);
        }
    }

    private static void write(FileOutputStream output, String text) throws IOException {
        output.write(text.getBytes(StandardCharsets.UTF_8));
    }
}
