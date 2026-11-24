class ModernMenuLighting extends ModernMenuPage;

function CreateMenuContents()
{
	local UWindowComboControl Combo;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernSunShadows", 3, 4);
	AddNames(Combo, 0, 4);
	AddLevelSlider(1, "ModernShadowDistance", 1, 5, 4);
	Combo = AddLevelCombo(2, "ModernLampShadows", 3, 4);
	AddNames(Combo, 0, 4);
	Combo = AddLevelCombo(3, "ModernContactShadows", 1, 2);
	AddNames(Combo, 0, 1);
	Combo.AddItem(LevelNames[3]);
	Combo = AddLevelCombo(4, "ModernAO", 2, 3);
	AddNames(Combo, 0, 3);
	Combo = AddLevelCombo(5, "ModernIndirect", 2, 2);
	AddNames(Combo, 0, 1);
	Combo.AddItem(LevelNames[3]);
	AddBox(6, "ModernTerrainDetail");
	AddBack();
	LoadRows();
}

defaultproperties
{
	TitleText="Shadows and Lighting"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="Sun shadows"
	Labels(1)="Shadow distance"
	Labels(2)="Lamp shadows"
	Labels(3)="Contact shadows"
	Labels(4)="Ambient occlusion"
	Labels(5)="Indirect light"
	Labels(6)="Rounded terrain"

	Helps(0)="Shadows from the sun, cast by characters and moving objects. Higher is sharper."
	Helps(1)="How far away sun shadows are still drawn."
	Helps(2)="Shadows from lamps and other lights in the level."
	Helps(3)="Small shadows where things meet the ground and walls."
	Helps(4)="Darker corners, creases and gaps. Turns MSAA edge smoothing off."
	Helps(5)="Light that bounces off walls and floors into the shade."
	Helps(6)="Hills and slopes are drawn as smooth curves instead of flat facets. Applies when a level loads."
}
