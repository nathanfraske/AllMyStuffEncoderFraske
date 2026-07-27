[CmdletBinding()]
param(
    [string]$OutputPath = (
        Join-Path $env:USERPROFILE (
            '.allmystuff-sandbox-stage\outbox\' +
            'windows-display-capture-probe.json'
        )
    )
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

public static class AllMyStuffDisplayCaptureProbe
{
    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapInfoHeader
    {
        public UInt32 Size;
        public Int32 Width;
        public Int32 Height;
        public UInt16 Planes;
        public UInt16 BitCount;
        public UInt32 Compression;
        public UInt32 SizeImage;
        public Int32 XPelsPerMeter;
        public Int32 YPelsPerMeter;
        public UInt32 ClrUsed;
        public UInt32 ClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RgbQuad
    {
        public Byte Blue;
        public Byte Green;
        public Byte Red;
        public Byte Reserved;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapInfo
    {
        public BitmapInfoHeader Header;
        public RgbQuad Colors;
    }

    [DllImport("gdi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateDC(
        string driver,
        string device,
        string output,
        IntPtr initData);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern IntPtr CreateCompatibleDC(IntPtr dc);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern IntPtr CreateDIBSection(
        IntPtr dc,
        ref BitmapInfo info,
        UInt32 usage,
        out IntPtr bits,
        IntPtr section,
        UInt32 offset);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern IntPtr SelectObject(IntPtr dc, IntPtr value);

    [DllImport("gdi32.dll", SetLastError = true)]
    private static extern bool BitBlt(
        IntPtr destination,
        Int32 x,
        Int32 y,
        Int32 width,
        Int32 height,
        IntPtr source,
        Int32 sourceX,
        Int32 sourceY,
        UInt32 rasterOperation);

    [DllImport("gdi32.dll")]
    private static extern Int32 GetDeviceCaps(IntPtr dc, Int32 index);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteDC(IntPtr dc);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr value);

    private const Int32 HorzRes = 8;
    private const Int32 VertRes = 10;
    private const UInt32 DibRgbColors = 0;
    private const UInt32 SrcCopyCaptureBlt = 0x40CC0020;

    public static Dictionary<string, object> Capture(string device)
    {
        IntPtr display = IntPtr.Zero;
        IntPtr memory = IntPtr.Zero;
        IntPtr bitmap = IntPtr.Zero;
        IntPtr previous = IntPtr.Zero;
        try
        {
            display = CreateDC("DISPLAY", device, null, IntPtr.Zero);
            if (display == IntPtr.Zero)
                throw new InvalidOperationException(
                    "CreateDC failed: " + Marshal.GetLastWin32Error());

            int width = GetDeviceCaps(display, HorzRes);
            int height = GetDeviceCaps(display, VertRes);
            if (width <= 0 || height <= 0)
                throw new InvalidOperationException(
                    "invalid display dimensions: " + width + "x" + height);

            int byteCount = checked(width * height * 4);
            var info = new BitmapInfo();
            info.Header.Size = (UInt32)Marshal.SizeOf(typeof(BitmapInfoHeader));
            info.Header.Width = width;
            info.Header.Height = -height;
            info.Header.Planes = 1;
            info.Header.BitCount = 32;
            info.Header.SizeImage = (UInt32)byteCount;

            memory = CreateCompatibleDC(display);
            if (memory == IntPtr.Zero)
                throw new InvalidOperationException(
                    "CreateCompatibleDC failed: " + Marshal.GetLastWin32Error());

            IntPtr bits;
            bitmap = CreateDIBSection(
                display,
                ref info,
                DibRgbColors,
                out bits,
                IntPtr.Zero,
                0);
            if (bitmap == IntPtr.Zero || bits == IntPtr.Zero)
                throw new InvalidOperationException(
                    "CreateDIBSection failed: " + Marshal.GetLastWin32Error());

            previous = SelectObject(memory, bitmap);
            if (previous == IntPtr.Zero || previous == new IntPtr(-1))
                throw new InvalidOperationException(
                    "SelectObject failed: " + Marshal.GetLastWin32Error());

            if (!BitBlt(
                    memory,
                    0,
                    0,
                    width,
                    height,
                    display,
                    0,
                    0,
                    SrcCopyCaptureBlt))
                throw new InvalidOperationException(
                    "BitBlt failed: " + Marshal.GetLastWin32Error());

            var bgra = new byte[byteCount];
            Marshal.Copy(bits, bgra, 0, byteCount);
            long nonBlack = 0;
            long orange = 0;
            long purple = 0;
            var colors = new HashSet<Int32>();
            for (int offset = 0; offset < bgra.Length; offset += 4)
            {
                int blue = bgra[offset];
                int green = bgra[offset + 1];
                int red = bgra[offset + 2];
                if (red != 0 || green != 0 || blue != 0)
                    nonBlack++;
                if (red >= 245 && green >= 150 && green <= 180 && blue <= 15)
                    orange++;
                if (red >= 135 && red <= 160 &&
                    green >= 100 && green <= 125 &&
                    blue >= 205 && blue <= 230)
                    purple++;
                colors.Add((red << 16) | (green << 8) | blue);
            }
            string hash;
            using (SHA256 sha = SHA256.Create())
                hash = BitConverter.ToString(sha.ComputeHash(bgra)).Replace("-", "");

            return new Dictionary<string, object>
            {
                { "device", device },
                { "success", true },
                { "width", width },
                { "height", height },
                { "pixels", (long)width * height },
                { "non_black_pixels", nonBlack },
                { "unique_rgb_colors", colors.Count },
                { "orange_pixels", orange },
                { "purple_pixels", purple },
                { "sha256_bgra", hash }
            };
        }
        catch (Exception error)
        {
            return new Dictionary<string, object>
            {
                { "device", device },
                { "success", false },
                { "error", error.Message }
            };
        }
        finally
        {
            if (previous != IntPtr.Zero &&
                previous != new IntPtr(-1) &&
                memory != IntPtr.Zero)
                SelectObject(memory, previous);
            if (bitmap != IntPtr.Zero)
                DeleteObject(bitmap);
            if (memory != IntPtr.Zero)
                DeleteDC(memory);
            if (display != IntPtr.Zero)
                DeleteDC(display);
        }
    }
}
'@

$captures = @(
    [System.Windows.Forms.Screen]::AllScreens |
        ForEach-Object {
            [AllMyStuffDisplayCaptureProbe]::Capture($_.DeviceName)
        }
)

$result = [ordered]@{
    schema = 1
    kind = 'allmystuff-windows-display-capture-probe'
    host = $env:COMPUTERNAME
    session_id = (Get-Process -Id $PID).SessionId
    captured_utc = [DateTime]::UtcNow.ToString('o')
    captures = $captures
}
$json = $result | ConvertTo-Json -Depth 8 -Compress
if (-not [string]::IsNullOrWhiteSpace($OutputPath)) {
    $fullOutputPath = [IO.Path]::GetFullPath($OutputPath)
    $parent = Split-Path -Parent $fullOutputPath
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    [IO.File]::WriteAllText(
        $fullOutputPath,
        $json + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )
}
$json
