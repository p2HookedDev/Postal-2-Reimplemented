class ModernPhotoMode extends Interaction;

const KeyPath = "ini:Engine.Engine.RenderDevice";
const DefaultPhotoKey = "F3";
const Radius = 2000.0;
const MoveSpeed = 400.0;
const FastScale = 4.0;
const SpeedNeutral = 5;
const TurnPerCount = 16.0;
const MaxPitch = 16000;
const TiltMost = 45.0;
const TiltPerSecond = 45.0;
const HeightStep = 50.0;
const OrbitLeast = 64.0;
const FocusReach = 20000.0;
// The engine's focus distance is NearestFocus x 2^(level / 4) units.
const NearestFocus = 32.0;
const FocusTop = 36;
const BlurOnFocus = 5;
const GridAlpha = 110;
const StyleNormal = 1;
const StyleAlpha = 5;
const ShotDelayFrames = 2;
const HintHideTime = 0.3;
const HintFontIndex = 5;

var byte                PhotoKey;
var bool                bOpen;
var bool                bMenuShown;
var ModernPhotoCamera   Camera;
var Actor               OldViewTarget;
var bool                bOldBehindView;
var float               OldFovAngle;
var bool                bOldHideHUD;

var bool                bForward, bBack, bLeft, bRight, bUp, bSink, bRollLeft, bRollRight, bFast;
var bool                bShotPending;
var bool                bMenuAfterShot;
var int                 ShotFrames;
var float               HintBackTime;

var class<ModernMenuPhoto>  PageClass;
var float               Tilt;
var int                 SpeedLevel;
var bool                bGrid;
var int                 Bars;
var float               BarRatios[3];

var string              PictureKeys[8];
var string              PhotoKeys[8];
var int                 PictureHigh[8];

var localized string    HintText;

function PlayerController Viewer()
{
	if (ViewportOwner == None)
		return None;
	return ViewportOwner.Actor;
}

function ShellRootWindow MenuRoot()
{
	if (ViewportOwner == None || ViewportOwner.InteractionMaster == None)
		return None;
	return ShellRootWindow(ViewportOwner.InteractionMaster.BaseMenu);
}

function string ReadKey(string Key)
{
	return Viewer().ConsoleCommand("get" @ KeyPath @ Key);
}

function WriteKey(string Key, string Value)
{
	Viewer().ConsoleCommand("set" @ KeyPath @ Key @ Value);
}

function bool ModernOn()
{
	local string Value;

	Value = ReadKey("ModernGraphics");
	return Value ~= "True" || Value == "1";
}

function byte ReadPhotoKey()
{
	local string Name;
	local int Code;

	Name = ReadKey("ModernPhotoKey");
	if (Name != "")
		Code = int(Viewer().ConsoleCommand("KEYNO" @ Name));
	if (Code == 0)
		Code = int(Viewer().ConsoleCommand("KEYNO" @ DefaultPhotoKey));
	return byte(Code);
}

function int Nearest(float F)
{
	if (F < 0)
		return -int(0.5 - F);
	return int(F + 0.5);
}

function bool CanOpen()
{
	local PlayerController PC;

	PC = Viewer();
	if (PC == None || !ModernOn() || PC.Pawn == None || PC.Pawn.Health <= 0 || PC.myHUD == None)
		return false;
	return !PC.bInterpolating && !PC.Pawn.bInterpolating;
}

// Escape acts on release, as the retail menus do; its press is taken too, so neither half reaches a menu.
function bool KeyEvent(out EInputKey Key, out EInputAction Action, float Delta)
{
	if (!bOpen)
	{
		if (Action != IST_Press || Viewer() == None)
			return false;
		if (PhotoKey == 0)
			PhotoKey = ReadPhotoKey();
		return int(Key) == int(PhotoKey) && Open();
	}
	if (Key == IK_Escape)
	{
		if (Action == IST_Release)
			Close();
		return true;
	}
	if (Action == IST_Axis)
	{
		if (bMenuShown)
			return false;
		Turn(Key, Delta);
		return true;
	}
	if (Action == IST_Press && int(Key) == int(PhotoKey))
	{
		Close();
		return true;
	}
	if (Action == IST_Press && Key == IK_H)
	{
		if (!bShotPending && !bMenuAfterShot)
		{
			if (bMenuShown)
				HidePhotoMenu();
			else
				ShowPhotoMenu();
		}
		return true;
	}
	if (Action == IST_Press && Key == IK_LeftMouse && !bMenuShown)
	{
		TakePhoto();
		return true;
	}
	if ((Action == IST_Press || Action == IST_Release) && Hold(Key, Action == IST_Press))
		return true;
	return !bMenuShown;
}

function bool Open()
{
	local PlayerController PC;
	local ShellRootWindow Root;
	local Actor ViewActor;
	local vector CameraLocation;
	local rotator CameraRotation;

	PC = Viewer();
	Root = MenuRoot();
	if (!CanOpen() || PC.Level.Pauser != None || (Root != None && Root.IsMenuShowing()))
		return false;
	if (!PC.SetPause(true))
		return false;
	PC.PlayerCalcView(ViewActor, CameraLocation, CameraRotation);
	Camera = PC.Spawn(class'ModernPhotoCamera', PC,, CameraLocation, CameraRotation);
	if (Camera == None)
	{
		PC.SetPause(false);
		return false;
	}
	OldViewTarget = PC.ViewTarget;
	bOldBehindView = PC.bBehindView;
	OldFovAngle = PC.FovAngle;
	bOldHideHUD = PC.myHUD.bHideHUD;
	PC.bBehindView = false;
	PC.SetViewTarget(Camera);
	PC.myHUD.bHideHUD = true;
	ResetPicture();
	WriteKey("ModernPhotoActive", "1");
	ClearHeld();
	bShotPending = false;
	bMenuAfterShot = false;
	HintBackTime = 0;
	PageClass = class'ModernMenuPhotoCamera';
	bOpen = true;
	bVisible = true;
	bRequiresTick = true;
	SetFocus();
	ShowPhotoMenu();
	return true;
}

function Close()
{
	local PlayerController PC;

	if (bMenuShown)
		HidePhotoMenu();
	bOpen = false;
	bVisible = false;
	bRequiresTick = false;
	bShotPending = false;
	bMenuAfterShot = false;
	ClearHeld();
	PC = Viewer();
	if (PC != None)
	{
		if (OldViewTarget == None || OldViewTarget.bDeleteMe)
			OldViewTarget = PC.Pawn;
		PC.SetViewTarget(OldViewTarget);
		PC.bBehindView = bOldBehindView;
		PC.FovAngle = OldFovAngle;
		if (PC.myHUD != None)
			PC.myHUD.bHideHUD = bOldHideHUD;
		WriteKey("ModernPhotoActive", "0");
	}
	if (Camera != None)
		Camera.Destroy();
	Camera = None;
	OldViewTarget = None;
	if (PC != None)
		PC.SetPause(false);
}

function ResetPicture()
{
	local int i;
	local string Value;

	for (i = 0; i < ArrayCount(PictureKeys); i++)
	{
		Value = ReadKey(PictureKeys[i]);
		if (Value == "")
			Value = string(PictureHigh[i]);
		WriteKey(PhotoKeys[i], string(Clamp(int(Value), 0, 10)));
	}
	WriteKey("ModernPhotoFov", "0");
	WriteKey("ModernPhotoFocus", "0");
	WriteKey("ModernPhotoBlur", "0");
	WriteKey("ModernPhotoWarmth", "5");
	WriteKey("ModernPhotoHidePlayer", "0");
	Tilt = 0;
	SpeedLevel = SpeedNeutral;
	bGrid = false;
	Bars = 0;
}

function ShowPhotoMenu()
{
	local ShellRootWindow Root;

	Root = MenuRoot();
	if (Root == None)
		return;
	Root.bDontPauseOrUnPause = true;
	Root.ShowMenu();
	Root.bDontPauseOrUnPause = false;
	ShowPage(PageClass);
	bMenuShown = true;
}

function ShowPage(class<ModernMenuPhoto> NewPage)
{
	local ShellRootWindow Root;
	local ModernMenuPhoto Page;

	Root = MenuRoot();
	if (Root == None || NewPage == None)
		return;
	PageClass = NewPage;
	Root.bDontPauseOrUnPause = true;
	Root.CreateMenu(NewPage);
	Root.MyMenu.ShowWindow();
	Root.bDontPauseOrUnPause = false;
	Page = ModernMenuPhoto(Root.MyMenu);
	if (Page != None)
		Page.Attach(self);
}

function HidePhotoMenu()
{
	local ShellRootWindow Root;

	bMenuShown = false;
	Root = MenuRoot();
	if (Root == None)
		return;
	Root.bDontPauseOrUnPause = true;
	Root.HideMenu();
	Root.bDontPauseOrUnPause = false;
}

function TakePhoto()
{
	bMenuAfterShot = bMenuShown;
	if (bMenuShown)
		HidePhotoMenu();
	bShotPending = true;
	ShotFrames = 0;
}

function ResetCamera()
{
	local PlayerController PC;
	local rotator R;

	PC = Viewer();
	if (Camera == None || PC == None || PC.Pawn == None)
		return;
	R.Yaw = PC.Rotation.Yaw;
	Tilt = 0;
	Camera.SetLocation(PC.Pawn.Location + PC.Pawn.EyePosition());
	Camera.SetRotation(R);
}

function FocusOnCenter()
{
	local vector HitLocation, HitNormal, Start;
	local float Distance;
	local int Level;

	if (Camera == None)
		return;
	Start = Camera.Location;
	Level = FocusTop;
	if (Camera.Trace(HitLocation, HitNormal, Start + vector(Camera.Rotation) * FocusReach, Start, true) != None)
	{
		Distance = FMax(VSize(HitLocation - Start), 1.0);
		Level = Clamp(Nearest(4.0 * Loge(Distance / NearestFocus) / Loge(2.0)), 1, FocusTop);
	}
	WriteKey("ModernPhotoFocus", string(Level));
	if (int(ReadKey("ModernPhotoBlur")) <= 0)
		WriteKey("ModernPhotoBlur", string(BlurOnFocus));
}

function int OrbitDegrees()
{
	local vector Offset;
	local rotator Around;
	local int Yaw;

	if (Camera == None || Viewer().Pawn == None)
		return 0;
	Offset = Camera.Location - Viewer().Pawn.Location;
	Offset.Z = 0;
	Around = rotator(Offset);
	Yaw = (Around.Yaw - Viewer().Pawn.Rotation.Yaw) & 65535;
	if (Yaw > 32768)
		Yaw -= 65536;
	return Nearest(float(Yaw) * 360.0 / 65536.0);
}

function SetOrbit(int Degrees)
{
	local Pawn P;
	local vector Offset;
	local float Distance;
	local rotator R;

	P = Viewer().Pawn;
	if (Camera == None || P == None)
		return;
	Offset = Camera.Location - P.Location;
	Distance = FMax(Sqrt(Offset.X * Offset.X + Offset.Y * Offset.Y), OrbitLeast);
	R.Yaw = P.Rotation.Yaw + int(float(Degrees) * 65536.0 / 360.0);
	Offset = vector(R) * Distance + vect(0,0,1) * Offset.Z;
	Camera.SetLocation(P.Location + Offset);
	Camera.SetRotation(rotator(-Offset));
}

function int HeightLevel()
{
	if (Camera == None || Viewer().Pawn == None)
		return 0;
	return Nearest((Camera.Location.Z - Viewer().Pawn.Location.Z) / HeightStep);
}

function SetHeight(int Level)
{
	local vector At;

	if (Camera == None || Viewer().Pawn == None)
		return;
	At = Camera.Location;
	At.Z = Viewer().Pawn.Location.Z + Level * HeightStep;
	Camera.SetLocation(At);
}

function ClearHeld()
{
	bForward = false;
	bBack = false;
	bLeft = false;
	bRight = false;
	bUp = false;
	bSink = false;
	bRollLeft = false;
	bRollRight = false;
	bFast = false;
}

function bool Hold(EInputKey Key, bool bDown)
{
	local string Binding;
	local bool bFly;

	if (Key == IK_Shift)
	{
		bFast = bDown;
		return true;
	}
	if (Key == IK_Q)
	{
		bRollLeft = bDown;
		return true;
	}
	if (Key == IK_E)
	{
		bRollRight = bDown;
		return true;
	}
	Binding = Caps(Viewer().ConsoleCommand("KEYBINDING" @ Viewer().ConsoleCommand("KEYNAME" @ int(Key))));
	if (InStr(Binding, "MOVEFORWARD") >= 0)
	{
		bForward = bDown;
		bFly = true;
	}
	if (InStr(Binding, "MOVEBACKWARD") >= 0)
	{
		bBack = bDown;
		bFly = true;
	}
	if (InStr(Binding, "STRAFELEFT") >= 0)
	{
		bLeft = bDown;
		bFly = true;
	}
	if (InStr(Binding, "STRAFERIGHT") >= 0)
	{
		bRight = bDown;
		bFly = true;
	}
	if (InStr(Binding, "JUMP") >= 0)
	{
		bUp = bDown;
		bFly = true;
	}
	if (InStr(Binding, "DUCK") >= 0)
	{
		bSink = bDown;
		bFly = true;
	}
	return bFly;
}

function Turn(EInputKey Key, float Delta)
{
	local rotator R;

	if (Camera == None)
		return;
	R = Camera.Rotation;
	if (Key == IK_MouseX)
		R.Yaw += int(Delta * TurnPerCount);
	else if (Key == IK_MouseY)
	{
		R.Pitch = R.Pitch & 65535;
		if (R.Pitch > 32768)
			R.Pitch -= 65536;
		R.Pitch = Clamp(R.Pitch + int(Delta * TurnPerCount), -MaxPitch, MaxPitch);
	}
	Camera.SetRotation(R);
}

function Tick(float DeltaTime)
{
	local PlayerController PC;
	local float Real, Speed;
	local vector X, Y, Z, Move, Offset;
	local rotator R;

	PC = Viewer();
	if (!bOpen)
		return;
	if (Camera == None || PC == None || PC.Pawn == None)
	{
		Close();
		return;
	}
	if (bMenuAfterShot && !bShotPending && PC.Level.TimeSecondsAlways >= HintBackTime)
	{
		bMenuAfterShot = false;
		ShowPhotoMenu();
	}
	Real = DeltaTime / FMax(PC.Level.TimeDilation, 0.01);
	Speed = MoveSpeed * Real * SpeedLevel / SpeedNeutral;
	if (bFast)
		Speed *= FastScale;
	GetAxes(Camera.Rotation, X, Y, Z);
	if (bForward)
		Move += X;
	if (bBack)
		Move -= X;
	if (bRight)
		Move += Y;
	if (bLeft)
		Move -= Y;
	if (bUp)
		Move.Z += 1;
	if (bSink)
		Move.Z -= 1;
	Offset = Camera.Location - PC.Pawn.Location;
	if (Move != vect(0,0,0))
		Offset += Normal(Move) * Speed;
	if (VSize(Offset) > Radius)
		Offset = Normal(Offset) * Radius;
	Camera.SetLocation(PC.Pawn.Location + Offset);
	if (bRollLeft)
		Tilt -= TiltPerSecond * Real;
	if (bRollRight)
		Tilt += TiltPerSecond * Real;
	Tilt = FClamp(Tilt, -TiltMost, TiltMost);
	R = Camera.Rotation;
	R.Roll = int(Tilt * 65536.0 / 360.0);
	Camera.SetRotation(R);
}

function float BarHeight(Canvas Canvas)
{
	if (Bars <= 0 || Canvas.ClipY <= 0 || Canvas.ClipX / Canvas.ClipY >= BarRatios[Bars])
		return 0;
	return (Canvas.ClipY - Canvas.ClipX / BarRatios[Bars]) / 2;
}

function DrawBars(Canvas Canvas, float Height)
{
	Canvas.Style = StyleNormal;
	Canvas.SetDrawColor(0, 0, 0);
	Canvas.SetPos(0, 0);
	Canvas.DrawRect(Texture'UWindow.WhiteTexture', Canvas.ClipX, Height);
	Canvas.SetPos(0, Canvas.ClipY - Height);
	Canvas.DrawRect(Texture'UWindow.WhiteTexture', Canvas.ClipX, Height);
}

function DrawGrid(Canvas Canvas, float Top)
{
	local float Height;
	local int i;

	Height = Canvas.ClipY - Top * 2;
	Canvas.Style = StyleAlpha;
	Canvas.SetDrawColor(255, 255, 255);
	Canvas.DrawColor.A = GridAlpha;
	for (i = 1; i < 3; i++)
	{
		Canvas.SetPos(Canvas.ClipX * i / 3, Top);
		Canvas.DrawRect(Texture'UWindow.WhiteTexture', 1, Height);
		Canvas.SetPos(0, Top + Height * i / 3);
		Canvas.DrawRect(Texture'UWindow.WhiteTexture', Canvas.ClipX, 1);
	}
}

function PostRender(Canvas Canvas)
{
	local ShellRootWindow Root;
	local float XL, YL, X, Y, Top;

	Canvas.Reset();
	Top = BarHeight(Canvas);
	if (Top > 0)
		DrawBars(Canvas, Top);
	if (bShotPending)
	{
		ShotFrames++;
		if (ShotFrames > ShotDelayFrames)
		{
			bShotPending = false;
			Viewer().ConsoleCommand("shot");
			HintBackTime = Viewer().Level.TimeSecondsAlways + HintHideTime;
		}
		return;
	}
	if (bMenuAfterShot || Viewer().Level.TimeSecondsAlways < HintBackTime)
		return;
	if (bGrid)
		DrawGrid(Canvas, Top);
	if (bMenuShown)
		return;
	Root = MenuRoot();
	Canvas.Reset();
	Canvas.Font = Canvas.MedFont;
	if (Root != None && Root.Fonts[HintFontIndex] != None)
		Canvas.Font = Root.Fonts[HintFontIndex];
	Canvas.StrLen(HintText, XL, YL);
	X = Canvas.ClipX - XL - YL;
	Y = Canvas.ClipY - YL * 2;
	Canvas.SetDrawColor(10, 10, 10);
	if (Root != None && ShellLookAndFeel(Root.LookAndFeel) != None)
		Canvas.DrawColor = ShellLookAndFeel(Root.LookAndFeel).ShadowTextColor;
	Canvas.SetPos(X + 1, Y + 1);
	Canvas.DrawText(HintText);
	Canvas.SetDrawColor(160, 10, 10);
	if (Root != None && ShellLookAndFeel(Root.LookAndFeel) != None)
		Canvas.DrawColor = ShellLookAndFeel(Root.LookAndFeel).NormalTextColor;
	Canvas.SetPos(X, Y);
	Canvas.DrawText(HintText);
}

defaultproperties
{
	HintText="H: Show menu   Click: Take photo   Q and E: Tilt   Esc: Exit photo mode"

	PageClass=Class'ModernMenuPhotoCamera'
	SpeedLevel=5

	BarRatios(0)=0.0
	BarRatios(1)=1.85
	BarRatios(2)=2.35

	PictureKeys(0)="ModernExposure"
	PictureKeys(1)="ModernSaturation"
	PictureKeys(2)="ModernContrast"
	PictureKeys(3)="ModernVignette"
	PictureKeys(4)="ModernBloom"
	PictureKeys(5)="ModernGodRays"
	PictureKeys(6)="ModernChromaticAberration"
	PictureKeys(7)="ModernSharpen"

	PhotoKeys(0)="ModernPhotoExposure"
	PhotoKeys(1)="ModernPhotoSaturation"
	PhotoKeys(2)="ModernPhotoContrast"
	PhotoKeys(3)="ModernPhotoVignette"
	PhotoKeys(4)="ModernPhotoBloom"
	PhotoKeys(5)="ModernPhotoGodRays"
	PhotoKeys(6)="ModernPhotoFringe"
	PhotoKeys(7)="ModernPhotoSharpen"

	PictureHigh(0)=5
	PictureHigh(1)=5
	PictureHigh(2)=5
	PictureHigh(3)=2
	PictureHigh(4)=5
	PictureHigh(5)=5
	PictureHigh(6)=0
	PictureHigh(7)=4

	bActive=True
	bVisible=False
	bRequiresTick=False
}
