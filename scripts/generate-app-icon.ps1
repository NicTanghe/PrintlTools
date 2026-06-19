[CmdletBinding()]
param(
    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
    $OutputDirectory = Join-Path $scriptDirectory "..\assets\windows"
}

function Add-RoundedRectangle {
    param(
        [Parameter(Mandatory = $true)]
        [System.Drawing.Drawing2D.GraphicsPath]$Path,
        [Parameter(Mandatory = $true)]
        [System.Drawing.RectangleF]$Bounds,
        [Parameter(Mandatory = $true)]
        [float]$Radius
    )

    $diameter = $Radius * 2
    $Path.StartFigure()
    $Path.AddArc($Bounds.Left, $Bounds.Top, $diameter, $diameter, 180, 90)
    $Path.AddArc($Bounds.Right - $diameter, $Bounds.Top, $diameter, $diameter, 270, 90)
    $Path.AddArc(
        $Bounds.Right - $diameter,
        $Bounds.Bottom - $diameter,
        $diameter,
        $diameter,
        0,
        90
    )
    $Path.AddArc($Bounds.Left, $Bounds.Bottom - $diameter, $diameter, $diameter, 90, 90)
    $Path.CloseFigure()
}

function New-AppIconMaster {
    $bitmap = [System.Drawing.Bitmap]::new(
        1024,
        1024,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)

    try {
        $graphics.Clear([System.Drawing.Color]::Transparent)
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.CompositingQuality =
            [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.ScaleTransform(16, 16)

        $tilePath = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $tileBrush = [System.Drawing.SolidBrush]::new(
            [System.Drawing.ColorTranslator]::FromHtml("#146A9E")
        )
        try {
            Add-RoundedRectangle `
                -Path $tilePath `
                -Bounds ([System.Drawing.RectangleF]::new(4, 4, 56, 56)) `
                -Radius 10
            $graphics.FillPath($tileBrush, $tilePath)
        }
        finally {
            $tileBrush.Dispose()
            $tilePath.Dispose()
        }

        $paperPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $paperPen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 2.5)
        try {
            $paperPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
            $paperPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
            $paperPen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round

            $paperPath.StartFigure()
            $paperPath.AddLine(24, 16.5, 36.5, 16.5)
            $paperPath.AddBezier(36.5, 16.5, 37.2, 16.5, 37.8, 16.8, 38.3, 17.3)
            $paperPath.AddLine(38.3, 17.3, 46.7, 25.7)
            $paperPath.AddBezier(46.7, 25.7, 47.2, 26.2, 47.5, 26.8, 47.5, 27.5)
            $paperPath.AddLine(47.5, 27.5, 47.5, 46)
            $paperPath.AddBezier(47.5, 46, 47.5, 47.1, 46.6, 48, 45.5, 48)
            $paperPath.AddLine(45.5, 48, 24, 48)
            $paperPath.AddBezier(24, 48, 22.9, 48, 22, 47.1, 22, 46)
            $paperPath.AddLine(22, 46, 22, 18.5)
            $paperPath.AddBezier(22, 18.5, 22, 17.4, 22.9, 16.5, 24, 16.5)
            $paperPath.CloseFigure()
            $graphics.DrawPath($paperPen, $paperPath)

            $foldPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
            try {
                $foldPath.StartFigure()
                $foldPath.AddLine(38, 18, 38, 25)
                $foldPath.AddBezier(38, 25, 38, 26.1, 38.9, 27, 40, 27)
                $foldPath.AddLine(40, 27, 46, 27)
                $graphics.DrawPath($paperPen, $foldPath)
            }
            finally {
                $foldPath.Dispose()
            }
        }
        finally {
            $paperPen.Dispose()
            $paperPath.Dispose()
        }

        $detailPen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 2)
        try {
            $detailPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
            $detailPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
            $graphics.DrawLine($detailPen, 28.5, 33.5, 37.5, 33.5)
            $graphics.DrawLine($detailPen, 28.5, 39, 41.5, 39)
        }
        finally {
            $detailPen.Dispose()
        }
    }
    finally {
        $graphics.Dispose()
    }

    return $bitmap
}

function New-TrayIconMaster {
    $bitmap = [System.Drawing.Bitmap]::new(
        1024,
        1024,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)

    try {
        $graphics.Clear([System.Drawing.Color]::Transparent)
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.CompositingQuality =
            [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $graphics.ScaleTransform(16, 16)

        $tilePath = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $tileBrush = [System.Drawing.SolidBrush]::new(
            [System.Drawing.ColorTranslator]::FromHtml("#146A9E")
        )
        try {
            Add-RoundedRectangle `
                -Path $tilePath `
                -Bounds ([System.Drawing.RectangleF]::new(1, 1, 62, 62)) `
                -Radius 9
            $graphics.FillPath($tileBrush, $tilePath)
        }
        finally {
            $tileBrush.Dispose()
            $tilePath.Dispose()
        }

        $paperPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $paperPen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 3.2)
        try {
            $paperPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
            $paperPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
            $paperPen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round

            $paperPath.StartFigure()
            $paperPath.AddLine(21, 12.5, 36.5, 12.5)
            $paperPath.AddBezier(36.5, 12.5, 37.3, 12.5, 38, 12.8, 38.6, 13.4)
            $paperPath.AddLine(38.6, 13.4, 50.1, 24.9)
            $paperPath.AddBezier(50.1, 24.9, 50.7, 25.5, 51, 26.2, 51, 27)
            $paperPath.AddLine(51, 27, 51, 49.5)
            $paperPath.AddBezier(51, 49.5, 51, 50.9, 49.9, 52, 48.5, 52)
            $paperPath.AddLine(48.5, 52, 21, 52)
            $paperPath.AddBezier(21, 52, 19.6, 52, 18.5, 50.9, 18.5, 49.5)
            $paperPath.AddLine(18.5, 49.5, 18.5, 15)
            $paperPath.AddBezier(18.5, 15, 18.5, 13.6, 19.6, 12.5, 21, 12.5)
            $paperPath.CloseFigure()
            $graphics.DrawPath($paperPen, $paperPath)

            $foldPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
            try {
                $foldPath.StartFigure()
                $foldPath.AddLine(38, 14, 38, 24.5)
                $foldPath.AddBezier(38, 24.5, 38, 25.9, 39.1, 27, 40.5, 27)
                $foldPath.AddLine(40.5, 27, 49.5, 27)
                $graphics.DrawPath($paperPen, $foldPath)
            }
            finally {
                $foldPath.Dispose()
            }
        }
        finally {
            $paperPen.Dispose()
            $paperPath.Dispose()
        }

        $detailPen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 2.6)
        try {
            $detailPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
            $detailPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
            $graphics.DrawLine($detailPen, 26.5, 35.5, 39, 35.5)
            $graphics.DrawLine($detailPen, 26.5, 42, 44, 42)
        }
        finally {
            $detailPen.Dispose()
        }
    }
    finally {
        $graphics.Dispose()
    }

    return $bitmap
}

function Convert-ToPngBytes {
    param(
        [Parameter(Mandatory = $true)]
        [System.Drawing.Image]$Image
    )

    $stream = [System.IO.MemoryStream]::new()
    try {
        $Image.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        return $stream.ToArray()
    }
    finally {
        $stream.Dispose()
    }
}

function Export-IconFiles {
    param(
        [Parameter(Mandatory = $true)]
        [System.Drawing.Bitmap]$Master,
        [Parameter(Mandatory = $true)]
        [string]$BaseName,
        [Parameter(Mandatory = $true)]
        [string]$Destination
    )

    $sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
    $images = @()

    foreach ($size in $sizes) {
        $bitmap = [System.Drawing.Bitmap]::new(
            $size,
            $size,
            [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
        )
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.CompositingMode =
                [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
            $graphics.CompositingQuality =
                [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
            $graphics.InterpolationMode =
                [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.PixelOffsetMode =
                [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $graphics.DrawImage(
                $Master,
                [System.Drawing.Rectangle]::new(0, 0, $size, $size),
                0,
                0,
                $Master.Width,
                $Master.Height,
                [System.Drawing.GraphicsUnit]::Pixel
            )
        }
        finally {
            $graphics.Dispose()
        }

        try {
            $images += [PSCustomObject]@{
                Size = $size
                Bytes = [byte[]](Convert-ToPngBytes -Image $bitmap)
            }
        }
        finally {
            $bitmap.Dispose()
        }
    }

    $preview = $images | Where-Object Size -eq 256 | Select-Object -First 1
    [System.IO.File]::WriteAllBytes(
        (Join-Path $Destination "$BaseName.png"),
        $preview.Bytes
    )

    $iconPath = Join-Path $Destination "$BaseName.ico"
    $fileStream = [System.IO.File]::Create($iconPath)
    $writer = [System.IO.BinaryWriter]::new($fileStream)
    try {
        $writer.Write([uint16]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]$images.Count)

        $offset = 6 + (16 * $images.Count)
        foreach ($image in $images) {
            $dimension = if ($image.Size -ge 256) { 0 } else { $image.Size }
            $writer.Write([byte]$dimension)
            $writer.Write([byte]$dimension)
            $writer.Write([byte]0)
            $writer.Write([byte]0)
            $writer.Write([uint16]1)
            $writer.Write([uint16]32)
            $writer.Write([uint32]$image.Bytes.Length)
            $writer.Write([uint32]$offset)
            $offset += $image.Bytes.Length
        }

        foreach ($image in $images) {
            $writer.Write([byte[]]$image.Bytes)
        }
    }
    finally {
        $writer.Dispose()
        $fileStream.Dispose()
    }
}

$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
[System.IO.Directory]::CreateDirectory($resolvedOutput) | Out-Null

$appMaster = New-AppIconMaster
try {
    Export-IconFiles `
        -Master $appMaster `
        -BaseName "app-icon" `
        -Destination $resolvedOutput
}
finally {
    $appMaster.Dispose()
}

$trayMaster = New-TrayIconMaster
try {
    Export-IconFiles `
        -Master $trayMaster `
        -BaseName "tray-icon" `
        -Destination $resolvedOutput
}
finally {
    $trayMaster.Dispose()
}

Write-Host "Generated app and tray PNG/ICO assets in $resolvedOutput"
